#!/usr/bin/env python3
"""Deploy approved ARM64 images; local code/config only, never a remote runner.

compose.env belongs to Compose, not Python. --check only reads local state and
GitHub metadata. --self-check is offline. Default deployment requires the
owner-controlled automatic.enabled marker. Failed migrations require human review;
there is deliberately no automatic database restore or post-start rollback.
"""

import argparse
import contextlib
import fcntl
import gzip
import json
import os
from pathlib import Path
import re
import selectors
import signal
import stat
import subprocess
import sys
import tempfile
import time
import urllib.request

REPOSITORY = "Tiendepchai/graphwar"
SOURCE = "https://github.com/" + REPOSITORY
IMAGE = "ghcr.io/tiendepchai/graphwar"
PROJECT = "graphwar-home"
API = ("https://api.github.com/repos/" + REPOSITORY
       + "/actions/workflows/ci.yml/runs?branch=master&status=success&per_page=10")
DIGEST = re.compile(re.escape(IMAGE) + r"@sha256:[0-9a-f]{64}")
SHA = re.compile(r"[0-9a-f]{40}")


class UpdateError(Exception):
    pass


def require(condition, message):
    if not condition:
        raise UpdateError(message)


def private_path(path, directory=False):
    info = path.lstat()  # Reject symlinks, including the deployment directory.
    kind = stat.S_ISDIR if directory else stat.S_ISREG
    require(kind(info.st_mode) and info.st_uid == os.geteuid()
            and not info.st_mode & 0o077, "owner-private path required: " + path.name)


def atomic_write(path, text):
    fd, temporary = tempfile.mkstemp(prefix="." + path.name + ".", dir=path.parent)
    try:
        with os.fdopen(fd, "w", encoding="utf-8") as stream:
            stream.write(text)
            stream.flush()
            os.fsync(stream.fileno())
        os.replace(temporary, path)
        sync_directory(path.parent)
    finally:
        if os.path.exists(temporary):
            os.unlink(temporary)


def sync_directory(path):
    fd = os.open(path, os.O_RDONLY | os.O_DIRECTORY)
    try:
        os.fsync(fd)
    finally:
        os.close(fd)


@contextlib.contextmanager
def locked(directory):
    fd = os.open(directory / "update.lock", os.O_RDWR | os.O_CREAT | os.O_NOFOLLOW, 0o600)
    try:
        info = os.fstat(fd)
        require(stat.S_ISREG(info.st_mode) and info.st_uid == os.geteuid()
                and not info.st_mode & 0o077, "unsafe update.lock")
        try:
            fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            raise UpdateError("another updater holds update.lock") from None
        yield
    finally:
        os.close(fd)


def positive_int(value):
    return type(value) is int and value > 0


def valid_release(value):
    return (isinstance(value, dict) and positive_int(value.get("run_id"))
            and positive_int(value.get("attempt")) and isinstance(value.get("head_sha"), str)
            and SHA.fullmatch(value["head_sha"]) is not None)


def order(release):
    return release["run_id"], release["attempt"]


def select_release(payload):
    require(isinstance(payload, dict) and isinstance(payload.get("workflow_runs"), list),
            "invalid GitHub workflow response")
    releases = []
    for run in payload["workflow_runs"]:
        if not isinstance(run, dict):
            continue
        repository = run.get("head_repository")
        if (run.get("status") != "completed" or run.get("conclusion") != "success"
                or run.get("event") not in ("push", "workflow_dispatch")
                or run.get("head_branch") != "master" or not isinstance(repository, dict)
                or repository.get("full_name") != REPOSITORY):
            continue
        release = {"run_id": run.get("id"), "attempt": run.get("run_attempt"),
                   "head_sha": run.get("head_sha")}
        if valid_release(release):
            releases.append(release)
    require(releases, "no eligible successful master workflow run")
    return max(releases, key=order)


def discover():
    request = urllib.request.Request(API, headers={
        "Accept": "application/vnd.github+json", "X-GitHub-Api-Version": "2022-11-28",
        "User-Agent": "graphwar-home-updater"})
    try:
        with urllib.request.urlopen(request, timeout=30) as response:
            require(response.geturl() == API, "unexpected GitHub redirect")
            data = response.read(2_000_001)
        require(len(data) <= 2_000_000, "oversized GitHub response")
        return select_release(json.loads(data))
    except UpdateError:
        raise
    except Exception:
        raise UpdateError("GitHub discovery failed; deployment unchanged") from None


def load_state(directory):
    path = directory / "state.json"
    if not path.exists() and not path.is_symlink():
        return {"version": 1, "last_successful": None, "last_attempt": None}
    private_path(path)
    try:
        require(path.stat().st_size < 32_768, "state.json too large")
        state = json.loads(path.read_text(encoding="utf-8"))
        require(isinstance(state, dict) and state.get("version") == 1, "invalid state.json")
        for key in ("last_successful", "last_attempt"):
            require(key in state, "incomplete state.json")
            entry = state[key]
            if entry is None:
                continue
            require(valid_release(entry) and isinstance(entry.get("image"), str)
                    and DIGEST.fullmatch(entry["image"]), "invalid release in state.json")
            if key == "last_attempt":
                require(entry.get("status") in ("preparing", "starting", "failed", "succeeded")
                        and type(entry.get("start_attempted")) is bool, "invalid attempt status")
                previous = entry.get("previous_image")
                require(previous is None or (isinstance(previous, str) and DIGEST.fullmatch(previous)),
                        "invalid previous image")
                backup = entry.get("backup")
                require(backup is None or (isinstance(backup, str)
                        and re.fullmatch(r"backups/run-[0-9]+-[0-9]+-[0-9]+\.sql\.gz", backup)),
                        "invalid backup reference")
        if state["last_successful"]:
            require(state["last_attempt"] and order(state["last_successful"]) <= order(state["last_attempt"]),
                    "inconsistent state ordering")
        return state
    except (ValueError, TypeError, KeyError):
        raise UpdateError("invalid state.json; inspect manually") from None


def save_state(directory, state):
    atomic_write(directory / "state.json", json.dumps(state, sort_keys=True, indent=2) + "\n")


def eligible(release, state):
    return all(entry is None or order(release) > order(entry)
               for entry in (state["last_successful"], state["last_attempt"]))


def read_image(directory):
    path = directory / "image.env"
    if not path.exists() and not path.is_symlink():
        return None
    private_path(path)
    require(path.stat().st_size < 512, "invalid image.env")
    text = path.read_text(encoding="utf-8")
    match = re.fullmatch(r"GRAPHWAR_IMAGE=(.+)\n?", text)
    require(match is not None and DIGEST.fullmatch(match[1]), "image.env must contain one immutable image")
    return match[1]


def write_image(directory, image):
    require(DIGEST.fullmatch(image), "invalid immutable image")
    atomic_write(directory / "image.env", "GRAPHWAR_IMAGE=" + image + "\n")


class Docker:
    def __init__(self, directory):
        self.directory = directory
        # Compose interpolation must use its two files, never inherited overrides.
        self.env = {key: value for key, value in os.environ.items()
                    if not key.startswith(("COMPOSE_", "DOCKER_"))
                    and key not in ("DOMAIN", "POSTGRES_PASSWORD", "GRAPHWAR_IMAGE", "PGPASSWORD", "DATABASE_URL")}
        self.prefix = ["docker", "--host", "unix:///var/run/docker.sock"]
        self.compose = ["compose", "--project-name", PROJECT, "--project-directory", str(directory),
                        "--env-file", str(directory / "compose.env"),
                        "--env-file", str(directory / "image.env"),
                        "-f", str(directory / "compose.home.yaml")]

    def run(self, args, timeout=120):
        try:
            result = subprocess.run(self.prefix + args, cwd=self.directory, env=self.env,
                                    stdin=subprocess.DEVNULL, stdout=subprocess.PIPE,
                                    stderr=subprocess.DEVNULL, timeout=timeout, check=False)
            require(result.returncode == 0, "Docker operation failed: " + args[0])
            return result.stdout.decode("utf-8")
        except (OSError, UnicodeError, subprocess.TimeoutExpired):
            raise UpdateError("Docker operation unavailable/timed out: " + args[0]) from None

    def json(self, args):
        try:
            return json.loads(self.run(args))
        except ValueError:
            raise UpdateError("invalid Docker metadata") from None

    def app_command(self, *args):
        return self.run(self.compose + list(args), timeout=180)

    def container(self, identifier):
        # Never capture Config.Env (contains the database password) or health logs.
        template = ('{"id":{{json .Id}},"image":{{json .Config.Image}},'
                    '"labels":{{json .Config.Labels}},"running":{{json .State.Running}},'
                    '"status":{{json .State.Status}},"mounts":{{json .Mounts}},'
                    '"networks":{{json .NetworkSettings.Networks}},"health":'
                    '{{if .State.Health}}{{json .State.Health.Status}}{{else}}null{{end}}}')
        return self.json(["container", "inspect", "--format", template, identifier])

    def inventory(self, previous, state):
        identifiers = self.run(["container", "ls", "-aq", "--filter", "label=com.docker.compose.project=" + PROJECT]).split()
        services = {}
        for identifier in identifiers:
            container = self.container(identifier)
            labels = container.get("labels") or {}
            service = labels.get("com.docker.compose.service")
            require(service in ("app", "postgres") and service not in services
                    and labels.get("com.docker.compose.project") == PROJECT
                    and labels.get("com.docker.compose.project.working_dir") == str(self.directory)
                    and labels.get("com.docker.compose.project.config_files") == str(self.directory / "compose.home.yaml")
                    and labels.get("com.docker.compose.oneoff") == "False",
                    "unknown/conflicting graphwar-home container; inspect manually")
            services[service] = container
        require(not state["last_successful"] or "postgres" in services,
                "postgres missing from an existing deployment; inspect manually")
        app = services.get("app")
        if app:
            known = {entry.get(field) for entry in (state["last_successful"], state["last_attempt"])
                     if entry for field in ("image", "previous_image")}
            require(previous is not None and app["image"] == previous and previous in known,
                    "unknown app image; refusing overwrite")
            require("postgres" in services, "app exists without its postgres container")
            require(app["status"] in ("running", "exited", "created"), "app not in a stable state")
            require(not app["running"] or (state["last_attempt"] is not None
                    and (state["last_attempt"]["status"] == "succeeded"
                         or (state["last_attempt"]["status"] == "failed"
                             and not state["last_attempt"]["start_attempted"]))
                    and state["last_successful"] is not None
                    and previous == state["last_successful"]["image"]),
                    "unverified running app; inspect manually")
        if "postgres" in services:
            database = services["postgres"]
            require(database["image"] == "postgres:17-alpine", "unknown postgres image")
            mounts = [mount for mount in database["mounts"]
                      if mount.get("Destination") == "/var/lib/postgresql/data"]
            require(len(mounts) == 1 and mounts[0].get("Type") == "volume"
                    and mounts[0].get("Name") == PROJECT + "_postgres-data",
                    "postgres data volume mismatch; refusing backup/deploy")
        require(all(set(container["networks"]) == {PROJECT + "_database"}
                    for container in services.values()), "unexpected project network")
        # Refuse resource-name collisions or orphaned data; never adopt an old volume.
        for kind, name, label in (("volume", PROJECT + "_postgres-data", "volume"),
                                  ("network", PROJECT + "_database", "network")):
            names = self.run([kind, "ls", "--format", "{{.Name}}", "--filter", "name=" + name]).split()
            if name in names:
                labels = self.json([kind, "inspect", "--format", "{{json .Labels}}", name]) or {}
                require("postgres" in services and labels.get("com.docker.compose.project") == PROJECT
                        and labels.get("com.docker.compose." + label) == ("postgres-data" if kind == "volume" else "database"),
                        "unknown/orphaned graphwar-home " + kind)
            else:
                require(not services, "existing project resource missing; inspect manually")
        return services

    def candidate(self, release):
        tag = IMAGE + ":run-" + str(release["run_id"]) + "-" + str(release["attempt"])
        self.run(["image", "pull", "--platform", "linux/arm64", tag], timeout=600)
        template = ('{"architecture":{{json .Architecture}},"os":{{json .Os}},'
                    '"labels":{{json .Config.Labels}},"digests":{{json .RepoDigests}}}')
        metadata = self.json(["image", "inspect", "--format", template, tag])
        labels = metadata.get("labels") or {}
        require(metadata.get("architecture") == "arm64" and metadata.get("os") == "linux"
                and labels.get("org.opencontainers.image.source") == SOURCE
                and labels.get("org.opencontainers.image.revision") == release["head_sha"],
                "candidate architecture/source/revision mismatch; deployment unchanged")
        digests = metadata.get("digests")
        require(isinstance(digests, list), "candidate has no repository digest")
        matches = {digest for digest in digests if isinstance(digest, str) and DIGEST.fullmatch(digest)}
        require(len(matches) == 1, "candidate repository digest missing/ambiguous")
        image = matches.pop()
        # A tag may resolve to a multi-platform manifest; verify the exact digest too.
        exact = self.json(["image", "inspect", "--format", template, image])
        require(exact == metadata, "candidate digest metadata mismatch")
        return image

    def service(self, name):
        identifiers = self.app_command("ps", "-a", "-q", name).split()
        require(len(identifiers) == 1, "expected exactly one " + name + " container")
        return self.container(identifiers[0])

    def wait_healthy(self, name, seconds=150):
        deadline = time.monotonic() + seconds
        while time.monotonic() < deadline:
            container = self.service(name)
            require(container["running"] and container["status"] == "running"
                    and container["health"] != "unhealthy", name + " failed health check")
            if container["health"] == "healthy":
                return
            time.sleep(2)
        raise UpdateError(name + " health check timed out")

    def stop_app(self):
        self.app_command("stop", "--timeout", "30", "app")
        app = self.service("app")
        require(not app["running"] and app["status"] in ("exited", "created"),
                "app not confirmed stopped; backup/deploy refused")

    def backup(self, path):
        # Stream pg_dump directly to a private gzip; neither SQL nor stderr is logged.
        fd, temporary = tempfile.mkstemp(prefix=".dump-", dir=path.parent)
        process = None
        try:
            with os.fdopen(fd, "wb") as raw:
                process = subprocess.Popen(self.prefix + self.compose + ["exec", "-T", "postgres",
                                           "pg_dump", "-U", "graphwar", "-d", "graphwar"],
                                           cwd=self.directory, env=self.env, stdin=subprocess.DEVNULL,
                                           stdout=subprocess.PIPE, stderr=subprocess.DEVNULL)
                deadline = time.monotonic() + 600
                with selectors.DefaultSelector() as selector, gzip.GzipFile(filename="", fileobj=raw, mode="wb", mtime=0) as zipped:
                    selector.register(process.stdout, selectors.EVENT_READ)
                    while True:
                        require(time.monotonic() < deadline, "database backup timed out")
                        if not selector.select(timeout=min(1, max(0, deadline - time.monotonic()))):
                            continue
                        chunk = os.read(process.stdout.fileno(), 1024 * 1024)
                        if not chunk:
                            break
                        zipped.write(chunk)
                require(process.wait(timeout=max(1, deadline - time.monotonic())) == 0, "database backup failed")
                raw.flush()
                os.fsync(raw.fileno())
            with gzip.open(temporary, "rb") as zipped:
                size = 0
                while chunk := zipped.read(1024 * 1024):
                    size += len(chunk)
            require(size > 0, "empty database backup")
            require(not path.exists(), "backup already exists; refusing overwrite")
            os.replace(temporary, path)
            sync_directory(path.parent)
        finally:
            if process is not None:
                if process.poll() is None:
                    process.kill()
                process.wait()
                process.stdout.close()
            if os.path.exists(temporary):
                os.unlink(temporary)


def deploy(directory, docker, release, state, image, previous, services):
    backup_directory = directory / "backups"
    backup_directory.mkdir(mode=0o700, exist_ok=True)
    private_path(backup_directory, directory=True)
    prior_success = state["last_successful"]
    attempt = dict(release, image=image, previous_image=previous, backup=None,
                   status="preparing", start_attempted=False)
    state["last_attempt"] = attempt
    save_state(directory, state)  # Crash/interruption is a manual-review stop, not an implicit retry.
    app = services.get("app")
    was_running = bool(app and app["running"])
    candidate_started = False
    image_changed = False
    try:
        if previous is None:
            image_changed = True
            write_image(directory, image)  # First install needs interpolation even for postgres.
        if "postgres" not in services:
            docker.app_command("up", "-d", "--no-deps", "postgres")
        elif not services["postgres"]["running"]:
            docker.app_command("start", "postgres")
        docker.wait_healthy("postgres")
        if app:
            docker.stop_app()
        backup = backup_directory / ("run-" + str(release["run_id"]) + "-"
                                      + str(release["attempt"]) + "-" + str(time.time_ns()) + ".sql.gz")
        docker.backup(backup)
        attempt["backup"] = str(backup.relative_to(directory))
        attempt["status"] = "starting"
        attempt["start_attempted"] = True
        save_state(directory, state)
        image_changed = True
        write_image(directory, image)
        # From this point migrations MAY have executed, including if `up` itself fails.
        candidate_started = True
        docker.app_command("up", "-d", "--no-deps", "--pull", "never", "app")
        docker.wait_healthy("app")
        state["last_successful"] = dict(release, image=image)
        attempt["status"] = "succeeded"
        save_state(directory, state)
    except BaseException as error:
        # Cleanup never masks the original error or restores a possibly migrated DB.
        cleanup_failed = False
        try:
            if candidate_started:
                docker.stop_app()
            else:
                if image_changed:
                    if previous:
                        write_image(directory, previous)
                    else:
                        (directory / "image.env").unlink(missing_ok=True)
                        sync_directory(directory)
                if was_running:
                    docker.app_command("start", "app")
        except BaseException:
            cleanup_failed = True
        attempt["status"] = "failed"
        attempt["start_attempted"] = candidate_started
        state["last_successful"] = prior_success
        attempt["cleanup_failed"] = cleanup_failed
        try:
            save_state(directory, state)
        except BaseException:
            cleanup_failed = True
        if cleanup_failed:
            print("cleanup/state write failed; manual inspection required", file=sys.stderr)
        if isinstance(error, (KeyboardInterrupt, SystemExit)):
            raise
        raise UpdateError((str(error) if isinstance(error, UpdateError) else "deployment failed")
                          + ("; candidate start attempted, no database restore/rollback" if candidate_started
                             else "; candidate never started")) from None


def run(directory, check=False):
    private_path(directory, directory=True)
    if not check:
        marker = (directory / "automatic.enabled").lstat()
        require(stat.S_ISREG(marker.st_mode) and marker.st_uid == os.geteuid()
                and not marker.st_mode & 0o022, "unsafe automatic.enabled marker")
    with contextlib.nullcontext() if check else locked(directory):
        state = load_state(directory)
        require(not state["last_attempt"] or not state["last_attempt"].get("cleanup_failed"),
                "previous cleanup failed; inspect manually")
        require(not state["last_attempt"] or state["last_attempt"]["status"] not in ("preparing", "starting"),
                "interrupted deployment; inspect state, containers and backup manually")
        release = discover()
        label = "run-" + str(release["run_id"]) + "-" + str(release["attempt"])
        if not eligible(release, state):
            print("no update: " + label + " already attempted or older than local state")
            return
        if check:
            print("available: " + label + " " + release["head_sha"] + "; discovery only, no pull/deploy")
            return
        private_path(directory / "compose.env")  # Stat only; never read secrets.
        private_path(directory / "compose.home.yaml")
        previous = read_image(directory)
        require(previous is not None or not state["last_successful"], "image.env missing for existing deployment")
        docker = Docker(directory)
        image = docker.candidate(release)  # Pull/provenance verification before any downtime.
        services = docker.inventory(previous, state)
        deploy(directory, docker, release, state, image, previous, services)
        print("deployed: " + label + " " + image)


def self_check():
    # Kept in this installed file so the installer can verify it without extra files.
    from unittest.mock import patch
    import copy

    def fails(call):
        try:
            call()
        except (UpdateError, OSError):
            return
        raise AssertionError("expected refusal")

    good = {"id": 20, "run_attempt": 2, "head_sha": "a" * 40, "status": "completed",
            "conclusion": "success", "event": "push", "head_branch": "master",
            "head_repository": {"full_name": REPOSITORY}}
    invalid = [dict(good, id=999, event="pull_request"),
               dict(good, id=999, head_repository={"full_name": "fork/graphwar"}),
               dict(good, id=999, head_sha="bad"), dict(good, id=True),
               dict(good, id=999, status="in_progress"), dict(good, id=999, head_branch="feature"),
               dict(good, id=999, run_attempt=0), dict(good, id=999, conclusion="failure"),
               dict(good, id=999, event="pull_request_target"), dict(good, id="999"),
               dict(good, id=999, run_attempt=True), dict(good, id=999, head_repository=None)]
    release = select_release({"workflow_runs": invalid + [good, dict(good, id=19, run_attempt=99)]})
    assert order(release) == (20, 2)
    fails(lambda: select_release({"workflow_runs": invalid}))
    first = IMAGE + "@sha256:" + "1" * 64
    second = IMAGE + "@sha256:" + "2" * 64
    metadata = {"architecture": "arm64", "os": "linux",
                "labels": {"org.opencontainers.image.source": SOURCE,
                           "org.opencontainers.image.revision": release["head_sha"]},
                "digests": [second]}
    candidate_docker = Docker(Path("/"))
    with patch.object(candidate_docker, "run") as commands, patch.object(candidate_docker, "json", return_value=metadata):
        assert candidate_docker.candidate(release) == second
        assert commands.call_args.args[0] == ["image", "pull", "--platform", "linux/arm64", IMAGE + ":run-20-2"]
    bad_metadata = [dict(metadata, architecture="amd64"), dict(metadata, os="windows"),
                    dict(metadata, labels={"org.opencontainers.image.source": SOURCE,
                                           "org.opencontainers.image.revision": "b" * 40}),
                    dict(metadata, digests=["ghcr.io/fork/graphwar@sha256:" + "2" * 64]),
                    dict(metadata, digests=[first, second])]
    for bad in bad_metadata:
        with patch.object(candidate_docker, "run"), patch.object(candidate_docker, "json", return_value=bad):
            fails(lambda: candidate_docker.candidate(release))
    success = dict(release, run_id=19, image=first)
    state = {"version": 1, "last_successful": success, "last_attempt": dict(success, status="succeeded", previous_image=None, backup=None)}
    assert eligible(release, state)
    assert not eligible(dict(release, run_id=18, attempt=999), state)
    assert not eligible(dict(release, run_id=19, attempt=1), state)
    state["last_attempt"] = dict(release, image=second, status="failed", previous_image=first, backup=None)
    assert not eligible(release, state)
    assert eligible(dict(release, attempt=3), state)
    with patch.object(candidate_docker, "run", return_value=""):
        assert candidate_docker.inventory(None, {"last_successful": None, "last_attempt": None}) == {}
        fails(lambda: candidate_docker.inventory(first, state))

    class FakeDocker:
        def __init__(self, failure):
            self.failure = failure
            self.calls = []

        def app_command(self, *args):
            self.calls.append(args)

        def stop_app(self):
            self.calls.append(("stop", "app"))

        def wait_healthy(self, name):
            self.calls.append(("health", name))
            if self.failure == "health" and name == "app":
                raise UpdateError("app failed health check")
            if self.failure == "postgres" and name == "postgres":
                raise UpdateError("postgres failed health check")

        def backup(self, path):
            self.calls.append(("backup",))
            if self.failure == "backup":
                raise UpdateError("database backup failed")
            with gzip.open(path, "wb") as stream:
                stream.write(b"test SQL\n")

    with tempfile.TemporaryDirectory() as temporary:
        directory = Path(temporary)
        private_path(directory, directory=True)
        with patch(__name__ + ".discover", side_effect=AssertionError("must not contact GitHub")):
            fails(lambda: run(directory))  # Missing enable marker fails before any external work.
        atomic_write(directory / "probe", "old")
        with patch.object(os, "replace", side_effect=OSError("simulated disk error")):
            fails(lambda: atomic_write(directory / "probe", "new"))
        assert (directory / "probe").read_text() == "old"
        assert not list(directory.glob(".probe.*"))
        with locked(directory):
            fails(lambda: locked(directory).__enter__())
        with locked(directory):
            pass
        write_image(directory, first)
        assert read_image(directory) == first
        services = {"app": {"running": True}, "postgres": {"running": True}}
        for failure in ("backup", "health", None):
            current = {"version": 1, "last_successful": copy.deepcopy(success),
                       "last_attempt": dict(success, status="succeeded", previous_image=None, backup=None)}
            write_image(directory, first)
            fake = FakeDocker(failure)
            if failure:
                fails(lambda: deploy(directory, fake, release, current, second, first, services))
            else:
                deploy(directory, fake, release, current, second, first, services)
            saved = load_state(directory)
            assert not any("restore" in call or "down" in call for call in fake.calls)
            if failure == "backup":
                assert ("start", "app") in fake.calls
                assert not any(call[0] == "up" for call in fake.calls)
                assert read_image(directory) == first
                assert saved["last_successful"] == success
            elif failure == "health":
                assert ("start", "app") not in fake.calls
                assert fake.calls[-1] == ("stop", "app")
                assert read_image(directory) == second
                assert saved["last_successful"] == success
                with gzip.open(directory / saved["last_attempt"]["backup"], "rb") as stream:
                    assert stream.read()
                assert not eligible(release, saved)
            else:
                assert saved["last_successful"]["image"] == second
                assert saved["last_attempt"]["status"] == "succeeded"
        # Interrupted/invalid state never causes another migration attempt.
        (directory / "automatic.enabled").touch(mode=0o600)
        interrupted = copy.deepcopy(saved)
        interrupted["last_attempt"]["status"] = "starting"
        save_state(directory, interrupted)
        with patch(__name__ + ".discover", side_effect=AssertionError("interrupted state must stop")):
            fails(lambda: run(directory))
        save_state(directory, saved)
        before = {path.name: path.read_bytes() for path in directory.iterdir() if path.is_file()}
        with patch(__name__ + ".discover", return_value=dict(release, run_id=21)), patch(__name__ + ".Docker", side_effect=AssertionError("check must not use Docker")), patch("builtins.print"):
            run(directory, check=True)
        assert before == {path.name: path.read_bytes() for path in directory.iterdir() if path.is_file()}

        # Final state write failure retains the prior successful run, stops candidate.
        current = {"version": 1, "last_successful": copy.deepcopy(success),
                   "last_attempt": dict(success, status="succeeded", start_attempted=True,
                                        previous_image=None, backup=None)}
        fake = FakeDocker(None)
        real_save = save_state
        def fail_success_write(target, value):
            if value["last_attempt"]["status"] == "succeeded":
                raise OSError("disk full")
            real_save(target, value)
        with patch(__name__ + ".save_state", side_effect=fail_success_write):
            fails(lambda: deploy(directory, fake, release, current, second, first, services))
        assert load_state(directory)["last_successful"] == success
        assert fake.calls[-1] == ("stop", "app")
        assert ("start", "app") not in fake.calls

        # First installation creates only this project's postgres then backs it up.
        fresh = directory / "fresh"
        fresh.mkdir(mode=0o700)
        fake = FakeDocker(None)
        deploy(fresh, fake, release, {"version": 1, "last_successful": None,
               "last_attempt": None}, second, None, {})
        assert fake.calls[:3] == [("up", "-d", "--no-deps", "postgres"),
                                 ("health", "postgres"), ("backup",)]
        assert read_image(fresh) == second
        failed_fresh = directory / "failed-fresh"
        failed_fresh.mkdir(mode=0o700)
        fake = FakeDocker("postgres")
        fails(lambda: deploy(failed_fresh, fake, release, {"version": 1,
              "last_successful": None, "last_attempt": None}, second, None, {}))
        assert read_image(failed_fresh) is None
        assert ("backup",) not in fake.calls
        assert load_state(failed_fresh)["last_attempt"]["status"] == "failed"
    print("self-check: selection, provenance, enable/check gates, ordering, lock/atomic writes, backup/health/state failures, first install passed")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--directory", type=Path, default=Path(__file__).absolute().parent)
    mode = parser.add_mutually_exclusive_group()
    mode.add_argument("--check", action="store_true", help="GitHub discovery only; no writes, image pull or deploy")
    mode.add_argument("--self-check", action="store_true", help="offline mock safety checks")
    args = parser.parse_args()
    os.umask(0o077)
    def interrupted(signum, frame):
        raise KeyboardInterrupt
    signal.signal(signal.SIGTERM, interrupted)
    try:
        if args.self_check:
            self_check()
        else:
            run(args.directory.absolute(), args.check)
        return 0
    except KeyboardInterrupt:
        print("update interrupted; inspect local state before resuming", file=sys.stderr)
        return 130
    except UpdateError as error:
        print("update refused: " + str(error), file=sys.stderr)
    except (OSError, ValueError, TypeError, KeyError, subprocess.SubprocessError) as error:
        # Do not leak command output, environment values, SQL or credentials.
        print("update failed: " + type(error).__name__ + "; inspect local state", file=sys.stderr)
    return 1


if __name__ == "__main__":
    sys.exit(main())
