#!/usr/bin/env python3
"""Deterministic, two-session PostgreSQL claim tests against local dev Docker.

No third-party Python packages. A temporary database is created from the checked-in
migrations and always dropped afterwards. Never points at the normal aegis_flow DB.
"""

from __future__ import annotations

import queue
import re
import subprocess
import sys
import threading
import time
import uuid
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
COMPOSE_FILE = ROOT / "compose.postgres.yml"
MIGRATIONS = ROOT / "db" / "migrations"
DB_PREFIX = "aegis_concurrency_"
SESSION_TIMEOUT = 15.0


class TestFailure(RuntimeError):
    pass


def docker_psql(database: str) -> list[str]:
    """Connect only through the compose-managed local development container."""
    return [
        "docker", "compose", "-f", str(COMPOSE_FILE), "exec", "-T", "postgres",
        "psql", "-X", "-qAt", "-v", "ON_ERROR_STOP=1", "-P", "pager=off",
        "-U", "aegis_flow", "-d", database,
    ]


def run_command(command: list[str], *, stdin: str | None = None, timeout: float = 45.0) -> str:
    try:
        result = subprocess.run(
            command, cwd=ROOT, input=stdin, text=True, capture_output=True,
            timeout=timeout, check=False,
        )
    except (OSError, subprocess.TimeoutExpired) as exc:
        raise TestFailure(f"Command failed to start or timed out: {command[0]}: {exc}") from exc
    if result.returncode != 0:
        raise TestFailure(
            f"Command failed (exit {result.returncode}): {' '.join(command)}\n"
            f"stdout:\n{result.stdout}\nstderr:\n{result.stderr}"
        )
    return result.stdout.strip()


def run_sql(db: str, sql: str, *, timeout: float = 45.0) -> list[str]:
    output = run_command(docker_psql(db), stdin=sql, timeout=timeout)
    return [line.strip() for line in output.splitlines() if line.strip()]


def require(condition: bool, message: str) -> None:
    if not condition:
        raise TestFailure(message)


def one_value(db: str, sql: str) -> str:
    rows = run_sql(db, sql)
    require(len(rows) == 1, f"Expected 1 SQL row, got {rows!r} for: {sql}")
    return rows[0]


class Session:
    """A persistent psql connection: BEGIN/claim can remain open while B runs."""

    def __init__(self, db: str, name: str) -> None:
        self.name = name
        self.lines: queue.Queue[str | None] = queue.Queue()
        self.stderr_lines: list[str] = []
        try:
            self.proc = subprocess.Popen(
                docker_psql(db), cwd=ROOT, stdin=subprocess.PIPE,
                stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                text=True, bufsize=1,
            )
        except OSError as exc:
            raise TestFailure(f"Could not start {name}: {exc}") from exc
        assert self.proc.stdout is not None
        assert self.proc.stderr is not None
        threading.Thread(target=self._read_stdout, daemon=True).start()
        threading.Thread(target=self._read_stderr, daemon=True).start()

    def _read_stdout(self) -> None:
        assert self.proc.stdout is not None
        for line in self.proc.stdout:
            self.lines.put(line.strip())
        self.lines.put(None)

    def _read_stderr(self) -> None:
        assert self.proc.stderr is not None
        for line in self.proc.stderr:
            self.stderr_lines.append(line.rstrip())

    def send(self, sql: str, *, timeout: float = SESSION_TIMEOUT) -> list[str]:
        assert self.proc.stdin is not None
        marker = "__AEGIS_END_" + uuid.uuid4().hex + "__"
        if self.proc.poll() is not None:
            raise TestFailure(f"{self.name} exited early: {self.stderr_lines}")
        try:
            self.proc.stdin.write(sql.rstrip() + "\n")
            self.proc.stdin.write("\\echo " + marker + "\n")
            self.proc.stdin.flush()
        except (BrokenPipeError, OSError) as exc:
            raise TestFailure(f"Cannot send commands to {self.name}: {self.stderr_lines}") from exc

        result: list[str] = []
        deadline = time.monotonic() + timeout
        while True:
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                raise TestFailure(
                    f"{self.name} did not finish SQL in {timeout:g}s (possible lock wait); "
                    f"output={result!r}; stderr={self.stderr_lines!r}"
                )
            try:
                line = self.lines.get(timeout=remaining)
            except queue.Empty as exc:
                raise TestFailure(
                    f"{self.name} did not finish SQL in {timeout:g}s (possible lock wait); "
                    f"output={result!r}; stderr={self.stderr_lines!r}"
                ) from exc
            if line is None:
                # Give the background stderr reader a chance to consume final output.
                self.proc.wait(timeout=2)
                raise TestFailure(
                    f"{self.name} ended unexpectedly (exit={self.proc.returncode}); "
                    f"output={result!r}; stderr={self.stderr_lines!r}"
                )
            if line == marker:
                return result
            if line:
                result.append(line)

    def close(self) -> None:
        if self.proc.poll() is None:
            if self.proc.stdin is not None:
                try:
                    # An open transaction rolls back when this session disconnects.
                    self.proc.stdin.write("\\q\n")
                    self.proc.stdin.flush()
                except (OSError, BrokenPipeError):
                    pass
            try:
                self.proc.wait(timeout=3)  # Reader threads own stdout/stderr.
            except subprocess.TimeoutExpired:
                self.proc.terminate()
                try:
                    self.proc.wait(timeout=3)
                except subprocess.TimeoutExpired:
                    self.proc.kill()
                    self.proc.wait(timeout=3)
        for stream in (self.proc.stdin, self.proc.stdout, self.proc.stderr):
            if stream is not None and not stream.closed:
                stream.close()


def parse_claim(lines: list[str], *, label: str, expected: int) -> list[tuple[str, str]]:
    prefix = label + "|"
    require(
        all(line.startswith(prefix) for line in lines),
        f"Unexpected {label} output: {lines!r}",
    )
    claims: list[tuple[str, str]] = []
    for line in lines:
        parts = line.split("|")
        require(len(parts) == 3, f"Malformed claim record: {line!r}")
        claims.append((str(uuid.UUID(parts[1])), str(uuid.UUID(parts[2]))))
    require(
        len(claims) == expected,
        f"{label}: expected {expected} claim(s), got {len(claims)}: {claims!r}",
    )
    return claims


def claim_sql(worker: str, label: str) -> str:
    # Only fixed strings from this test are used in these SQL literals.
    return (
        f"SELECT '{label}|' || activity_id || '|' || lease_token "
        f"FROM aegis.claim_activity('{worker}', INTERVAL '5 minutes');"
    )


def assert_activity(db: str, aid: str, state: str, attempt: int, claims: int) -> None:
    rows = run_sql(
        db,
        "SELECT a.state || '|' || a.attempt || '|' || "
        "(SELECT count(*) FROM aegis.events e WHERE e.activity_id = a.id "
        "AND e.event_type = 'ActivityClaimed') "
        f"FROM aegis.activities a WHERE a.id = '{aid}'::uuid;",
    )
    require(rows == [f"{state}|{attempt}|{claims}"], f"Wrong final state for {aid}: {rows!r}")


def prepare_workflow(db: str, activity_offsets_seconds: list[int]) -> list[str]:
    wid = str(uuid.uuid4())
    aids = [str(uuid.uuid4()) for _ in activity_offsets_seconds]
    statements = [
        f"INSERT INTO aegis.workflows(id, state) VALUES ('{wid}'::uuid, 'Running');"
    ]
    for aid, offset in zip(aids, activity_offsets_seconds):
        statements.append(
            "INSERT INTO aegis.activities(id, workflow_id, effect_policy, available_at) "
            f"VALUES ('{aid}'::uuid, '{wid}'::uuid, 'SafeToRetry', "
            f"clock_timestamp() - INTERVAL '{offset} seconds');"
        )
    run_sql(db, "BEGIN;\n" + "\n".join(statements) + "\nCOMMIT;")
    return aids


def test_single_activity(db: str, a: Session, b: Session) -> None:
    """While A's claim is UNCOMMITTED, B must skip its locked row."""
    (activity_id,) = prepare_workflow(db, [60])
    result = a.send("BEGIN;\n" + claim_sql("worker-a", "A1"))
    [(a_id, a_token)] = parse_claim(result, label="A1", expected=1)
    require(a_id == activity_id, f"A claimed wrong activity: {a_id}")

    # PostgreSQL's lock timeout makes a blocking SELECT fail rather than hang.
    # A is intentionally kept inside its transaction until B has responded.
    b_result = b.send(claim_sql("worker-b", "B1"))
    parse_claim(b_result, label="B1", expected=0)
    a.send("COMMIT;")
    parse_claim(b.send(claim_sql("worker-b", "B1_POST")), label="B1_POST", expected=0)

    wrong_token = str(uuid.uuid4())
    denied = one_value(
        db,
        f"SELECT aegis.finish_activity('{activity_id}', '{wrong_token}', 'Success');",
    )
    require(denied == "f", f"Unowned completion unexpectedly succeeded: {denied}")
    finished = one_value(
        db,
        f"SELECT aegis.finish_activity('{activity_id}', '{a_token}', 'Success');",
    )
    require(finished == "t", f"Owner could not finish its committed claim: {finished}")
    assert_activity(db, activity_id, "Succeeded", 1, 1)
    print("PASS: uncommitted claim skips locked single row; no double-claim")
    print("PASS: committed lease excludes a second worker; wrong token rejected")


def test_two_activities(db: str, a: Session, b: Session) -> None:
    """With two due rows, B can make progress while A holds the first lock."""
    older_id, newer_id = prepare_workflow(db, [120, 60])
    result = a.send("BEGIN;\n" + claim_sql("worker-a", "A2"))
    [(a_id, a_token)] = parse_claim(result, label="A2", expected=1)
    require(a_id == older_id, f"A did not claim the first due activity: {a_id}")

    # B's autocommit SELECT executes in another live PostgreSQL connection.
    # It must select the second activity BEFORE A commits the first.
    result = b.send(claim_sql("worker-b", "B2"))
    [(b_id, b_token)] = parse_claim(result, label="B2", expected=1)
    require(b_id == newer_id and a_id != b_id, f"Workers did not claim distinct rows: {a_id}, {b_id}")
    a.send("COMMIT;")
    parse_claim(b.send(claim_sql("worker-b", "B2_POST")), label="B2_POST", expected=0)

    for aid, token in ((a_id, a_token), (b_id, b_token)):
        ok = one_value(db, f"SELECT aegis.finish_activity('{aid}', '{token}', 'Success');")
        require(ok == "t", f"Finishing {aid} failed: {ok}")
        assert_activity(db, aid, "Succeeded", 1, 1)
    print("PASS: two live sessions claim separate rows while their transactions overlap")
    print("PASS: exactly one claim event per activity and no extra claims")


def main() -> int:
    if not COMPOSE_FILE.is_file():
        raise TestFailure("Run from a repository with compose.postgres.yml (Step 6A).")
    required = [MIGRATIONS / "0001_init.sql"]
    for path in required:
        require(path.is_file(), f"Required migration missing: {path}")
    run_command(
        ["docker", "compose", "-f", str(COMPOSE_FILE), "exec", "-T", "postgres",
         "pg_isready", "-U", "aegis_flow", "-d", "aegis_flow"],
        timeout=15,
    )
    database = DB_PREFIX + uuid.uuid4().hex[:16]
    require(bool(re.fullmatch(r"aegis_concurrency_[0-9a-f]{16}", database)), "Unsafe database name")
    created = False
    sessions: list[Session] = []
    primary_failure: Exception | None = None
    try:
        run_sql("postgres", f"CREATE DATABASE {database};")
        created = True
        print(f"Temporary test database created: {database}")
        # Test the repository's committed migrations, not an accidentally
        # patched function left over in the normal local database.
        for migration in sorted(MIGRATIONS.glob("*.sql")):
            print(f"Applying {migration.relative_to(ROOT)}")
            run_sql(database, migration.read_text(encoding="utf-8"))
        print("Migration chain: PASS")
        a, b = Session(database, "worker-a"), Session(database, "worker-b")
        sessions.extend((a, b))
        b.send("SET lock_timeout = '1500ms'; SET statement_timeout = '3s';")
        test_single_activity(database, a, b)
        test_two_activities(database, a, b)
        print("ALL CONCURRENCY TESTS PASSED")
        return 0
    except Exception as exc:
        primary_failure = exc
        raise
    finally:
        for session in sessions:
            session.close()
        if created:
            try:
                # Guard against accidentally dropping an existing/user database.
                require(
                    bool(re.fullmatch(r"aegis_concurrency_[0-9a-f]{16}", database)),
                    "Refusing to drop a database with an unexpected name",
                )
                run_sql("postgres", f"DROP DATABASE {database} WITH (FORCE);")
                print(f"Temporary test database removed: {database}")
            except Exception as cleanup_exc:
                print(f"WARNING: temporary database cleanup failed: {cleanup_exc}", file=sys.stderr)
                if primary_failure is None:
                    raise


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (TestFailure, ValueError) as exc:
        print(f"FAIL: {exc}", file=sys.stderr)
        raise SystemExit(1) from exc
