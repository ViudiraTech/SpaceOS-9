#!/usr/bin/env python3
#
#       tools/overlay_p5_packages.py
#       Add the signed local package repository and spk runtime to desktop stage
#
#       2026/11/21 By JiTianYu391
#       Copyright (C) 2026 ViudiraTech.
#

"""Overlay P5 package-manager assets onto a desktop rootfs stage."""

import argparse
import json
import os
import pathlib
import shutil
import subprocess
import sys
import tempfile

ROOT = pathlib.Path(__file__).resolve().parent.parent


def write(path: pathlib.Path, data: str, mode: int = 0o644) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(data)
    path.chmod(mode)


def copy(src: pathlib.Path, dst: pathlib.Path, mode: int) -> None:
    if not src.is_file():
        raise SystemExit(f"missing {src}")
    dst.parent.mkdir(parents=True, exist_ok=True)
    shutil.copy2(src, dst)
    dst.chmod(mode)


def install_runtime(
    stage: pathlib.Path,
    local_repo: pathlib.Path,
    spk: pathlib.Path,
    spk_host: pathlib.Path,
    repo_key: pathlib.Path,
) -> None:
    """Install base/sudo/spk-runtime from a filtered build repository so the
    desktop image gets real package DB entries without pulling base-core."""
    if not spk.is_file():
        raise SystemExit(f"missing host spk binary: {spk}")
    if not spk_host.is_file():
        raise SystemExit(f"missing host spk-host binary: {spk_host}")
    if not repo_key.is_file():
        raise SystemExit(f"missing build signing key: {repo_key}")
    runtime_repo = pathlib.Path(tempfile.mkdtemp(prefix="spk-runtime-repo-"))
    trusted = local_repo.parent / "trusted"
    trusted_dir = stage / "etc/spk/trusted.d"
    try:
        index = json.loads((local_repo / "index.json").read_text())
        wanted = {"base", "spk-runtime", "sudo"}
        selected = [pkg for pkg in index.get("packages", []) if pkg.get("name") in wanted]
        if {pkg["name"] for pkg in selected} != wanted:
            raise SystemExit(f"repository {local_repo} is missing base/spk-runtime/sudo")
        for package in selected:
            source = local_repo / package["filename"]
            if not source.is_file():
                raise SystemExit(f"missing package file: {source}")
            shutil.copy2(source, runtime_repo / source.name)
        subprocess.run(
            [
                str(spk_host),
                "index",
                "--repo-dir",
                str(runtime_repo),
                "--repo",
                "local",
                "--arch",
                "x86_64",
                "--key",
                str(repo_key),
            ],
            check=True,
        )

        shutil.rmtree(trusted_dir, ignore_errors=True)
        trusted_dir.mkdir(parents=True)
        for public in sorted(trusted.glob("*.pub")):
            shutil.copy2(public, trusted_dir / public.name)
            (trusted_dir / public.name).chmod(0o644)
        command = [
            "unshare",
            "-r",
            "--",
            str(spk),
            "--root",
            str(stage),
            "--noconfirm",
            "install",
            "--repo",
            str(runtime_repo),
            "--overwrite",
            "/usr/bin/spk",
            "--overwrite",
            "/usr/bin/spk-sandbox",
            "--overwrite",
            "/usr/bin/sudo",
            "--overwrite",
            "/usr/bin/doas",
            "spk-runtime",
            "base",
            "sudo",
        ]
        # A stage reused from an earlier build may hold the previous runtime
        # package whose old conflicts block the new transaction.
        installed_path = stage / "var/lib/spk/installed.json"
        if installed_path.is_file():
            try:
                installed = json.loads(installed_path.read_text())
            except json.JSONDecodeError:
                installed = {}
            packages = installed.get("packages", {})
            for name in ("base", "sudo", "spk-runtime"):
                if name in packages:
                    subprocess.run(
                        [
                            "unshare",
                            "-r",
                            "--",
                            str(spk),
                            "--root",
                            str(stage),
                            "--noconfirm",
                            "remove",
                            name,
                        ],
                        check=True,
                    )
        subprocess.run(command, check=True)
    finally:
        shutil.rmtree(trusted_dir, ignore_errors=True)
        shutil.rmtree(runtime_repo, ignore_errors=True)


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--stage", type=pathlib.Path, default=ROOT / "build/rootfs/stage")
    parser.add_argument("--local-repo", type=pathlib.Path,
                        help="optional local repository to embed for offline tests")
    parser.add_argument("--repo-name", default="spaceos-main")
    parser.add_argument("--repo-url", default="https://goutoustdio.top/spaceos/repo/x86_64")
    parser.add_argument("--spk", type=pathlib.Path,
                        default=ROOT / "target/x86_64-unknown-linux-musl/release/spk")
    parser.add_argument("--spk-host", type=pathlib.Path,
                        default=ROOT / "target/release/spk-host")
    parser.add_argument("--repo-key", type=pathlib.Path,
                        default=ROOT / "build/keys/repo.key")
    parser.add_argument("--sandbox", type=pathlib.Path,
                        default=ROOT / "target/x86_64-unknown-linux-musl/release/spk-sandbox")
    parser.add_argument("--trust-key", type=pathlib.Path,
                        default=ROOT / "packages/keys/ed25519:b4280a08406a53c5.pub")
    parser.add_argument("--ca-bundle", type=pathlib.Path, default=pathlib.Path("/etc/ssl/certs/ca-certificates.crt"))
    parser.add_argument("--epoch", type=int, default=0)
    arguments = parser.parse_args()
    stage = arguments.stage
    if not stage.is_dir():
        raise SystemExit(f"desktop stage missing: {stage}")
    if arguments.local_repo is not None and not arguments.local_repo.is_dir():
        raise SystemExit(f"local package repository missing: {arguments.local_repo}")

    if arguments.local_repo is not None:
        install_runtime(
            stage,
            arguments.local_repo,
            arguments.spk,
            arguments.spk_host,
            arguments.repo_key,
        )
    else:
        copy(arguments.spk, stage / "usr/bin/spk", 0o755)
        copy(arguments.sandbox, stage / "usr/bin/spk-sandbox", 0o755)
    if arguments.ca_bundle.is_file():
        copy(arguments.ca_bundle, stage / "etc/ssl/certs/ca-certificates.crt", 0o644)

    key = json.loads(arguments.trust_key.read_text())
    if "secret_key" in key:
        raise SystemExit(f"refusing to embed a private key: {arguments.trust_key}")
    write(
        stage / "etc/spk/trusted.json",
        json.dumps({"schema": 1, "keys": [key]}, indent=2) + "\n",
    )
    write(
        stage / "etc/spk/setuid.allow",
        "/usr/bin/sudo\n/usr/bin/doas\n",
        0o644,
    )
    write(
        stage / "var/lib/spk/repos.json",
        json.dumps(
            {
                "schema": 1,
                "repos": [
                    {
                        "name": arguments.repo_name,
                        "arch": "x86_64",
                        "urls": [arguments.repo_url],
                        "priority": 10,
                        "key_id": key["key_id"],
                        "last_sync": arguments.epoch,
                        "enabled": True,
                    }
                ],
            },
            indent=2,
        )
        + "\n",
    )
    # The desktop demo userland ships an unowned hello; a package install
    # would otherwise be rejected as an unowned file conflict.
    for path in ("usr/bin/hello", "bin/hello"):
        (stage / path).unlink(missing_ok=True)
    target = stage / "var/lib/spk/repo/x86_64"
    if target.exists():
        shutil.rmtree(target)
    if arguments.local_repo is not None:
        target.mkdir(parents=True)
        for item in sorted(arguments.local_repo.iterdir(), key=lambda path: path.name):
            if item.is_file():
                shutil.copy2(item, target / item.name)
                (target / item.name).chmod(0o644)
        print(f"embedded local repository {arguments.local_repo} into {stage}")
    # A previous local-validation overlay may have installed this unit.
    (stage / "etc/systemg/spk-verify.service").unlink(missing_ok=True)
    print(f"desktop repository URL: {arguments.repo_url}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
