#!/usr/bin/env python3
"""Package the portable xavi SDK and host generators using only Python's stdlib."""
import argparse
import hashlib
import json
from pathlib import Path
import re
import subprocess
from zipfile import ZIP_DEFLATED, ZipFile, ZipInfo

ROOT = Path(__file__).resolve().parents[1]
TARGETS = [
    "aarch64-apple-darwin", "x86_64-apple-darwin",
    "aarch64-unknown-linux-gnu", "x86_64-unknown-linux-gnu",
    "x86_64-pc-windows-msvc",
    "aarch64-apple-ios", "aarch64-apple-ios-sim",
    "aarch64-linux-android", "armv7-linux-androideabi", "x86_64-linux-android",
]
HOSTS = ["macos-aarch64", "macos-x86_64", "linux-aarch64", "linux-x86_64", "windows-x86_64"]


def git(root, *args):
    return subprocess.check_output(["git", "-C", str(root), *args], text=True).strip()


def validate_tag(tag):
    if tag != "nightly" and not re.fullmatch(r"v[0-9]+\.[0-9]+\.[0-9]+(?:-[0-9A-Za-z.-]+)?", tag):
        raise ValueError("release tag must be nightly or v<major>.<minor>.<patch>[-suffix]")
    return tag


def pinned_idl(root=ROOT, checkout=None):
    source = json.loads((root / "release-sources.json").read_text())["x-idl"]
    if source["repository"] != "rayzor-blade/x-idl" or not re.fullmatch(r"[0-9a-f]{40}", source["revision"]):
        raise ValueError("x-idl must be pinned to a full commit in rayzor-blade/x-idl")
    if checkout is not None:
        if git(checkout, "rev-parse", "HEAD") != source["revision"]:
            raise ValueError("x-idl checkout does not match release-sources.json")
        if git(checkout, "status", "--porcelain", "--untracked-files=all"):
            raise ValueError("x-idl checkout must be clean")
    return source


def sources(root, entries):
    files = {}
    for entry in entries:
        path = root / entry
        if not path.exists():
            raise ValueError(f"missing release input: {path}")
        for file in sorted(path.rglob("*")) if path.is_dir() else [path]:
            if file.is_symlink():
                raise ValueError(f"symlink in release inputs: {file}")
            if file.is_file() and "__pycache__" not in file.parts and file.suffix != ".pyc":
                files[file.relative_to(root).as_posix()] = file.read_bytes()
    return files


def write_zip(path, files, executable=None):
    path.parent.mkdir(parents=True, exist_ok=True)
    # Stable order, timestamps and modes make repeat packaging reproducible.
    with ZipFile(path, "w", compression=ZIP_DEFLATED) as archive:
        for name, data in sorted(files.items()):
            info = ZipInfo(name, (2020, 1, 1, 0, 0, 0))
            info.create_system = 3
            info.external_attr = (0o100755 if name == executable else 0o100644) << 16
            info.compress_type = ZIP_DEFLATED
            archive.writestr(info, data)
    return path


def sdk(output, tag, root=ROOT, idl=None, allow_dirty=False):
    validate_tag(tag)
    idl = idl or root.parent / "x-idl"
    pin = pinned_idl(root, idl)
    dirty = bool(git(root, "status", "--porcelain", "--untracked-files=all"))
    if dirty and not allow_dirty:
        raise ValueError("xavi checkout must be clean (use --allow-dirty only for local packaging tests)")
    files = {f"xavi/{name}": data for name, data in sources(root, [
        "Cargo.toml", "Cargo.lock", "README.md", "release-sources.json", "api", "crates", "examples",
    ]).items()}
    files.update({f"x-idl/{name}": data for name, data in sources(idl, [
        "Cargo.toml", "Cargo.lock", "README.md", "LICENSE", "src", "tests",
    ]).items()})
    manifest = {
        "schema": 1, "tag": tag, "revision": git(root, "rev-parse", "HEAD"),
        "dirty": dirty, "x-idl": pin, "targets": TARGETS,
        "files": {name: hashlib.sha256(data).hexdigest() for name, data in sorted(files.items())},
    }
    files["xavi-sdk.json"] = (json.dumps(manifest, indent=2) + "\n").encode()
    return write_zip(output / "xavi-sdk.zip", files)


def generator(output, platform, binary, generated, tag):
    validate_tag(tag)
    if platform not in HOSTS or not binary.is_file() or not binary.stat().st_size:
        raise ValueError("missing generator binary or unsupported host platform")
    for runtime in ["ash", "rayzor"]:
        for resource in ["AudioData", "VideoFrame", "MediaPlayer", "AudioEqualizer"]:
            if not (generated / runtime / "media" / f"{resource}.hx").is_file():
                raise ValueError(f"missing generated {runtime} {resource}")
    name = "bin/xavi-haxe.exe" if platform.startswith("windows") else "bin/xavi-haxe"
    files = {name: binary.read_bytes(), "README.md": (ROOT / "README.md").read_bytes()}
    files.update({f"haxe/{path}": data for path, data in sources(generated, ["ash", "rayzor"]).items()})
    metadata = {"schema": 1, "tag": tag, "revision": git(ROOT, "rev-parse", "HEAD"), "platform": platform}
    files["xavi-tools.json"] = (json.dumps(metadata, indent=2) + "\n").encode()
    return write_zip(output / f"xavi-tools-{platform}.zip", files, executable=name)


def checksums(directory, complete=False):
    files = sorted(directory.glob("*.zip"))
    expected = {"xavi-sdk.zip", *(f"xavi-tools-{host}.zip" for host in HOSTS)}
    if not files or (complete and {p.name for p in files} != expected):
        raise ValueError("release must contain the SDK and every host generator package")
    output = directory / "SHA256SUMS"
    output.write_text("".join(f"{hashlib.sha256(p.read_bytes()).hexdigest()}  {p.name}\n" for p in files))
    return output


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="command", required=True)
    p = sub.add_parser("pins")
    p.add_argument("--checkouts", action="store_true")
    p = sub.add_parser("sdk")
    p.add_argument("--output", type=Path, default=ROOT / "target/release-assets")
    p.add_argument("--tag", default="nightly")
    p.add_argument("--allow-dirty", action="store_true")
    p = sub.add_parser("tools")
    p.add_argument("--output", type=Path, default=ROOT / "target/release-assets")
    p.add_argument("--tag", default="nightly")
    p.add_argument("--platform", required=True, choices=HOSTS)
    p.add_argument("--binary", required=True, type=Path)
    p.add_argument("--generated", required=True, type=Path)
    p = sub.add_parser("checksums")
    p.add_argument("directory", type=Path)
    p.add_argument("--complete", action="store_true")
    args = parser.parse_args()
    if args.command == "pins":
        print(json.dumps(pinned_idl(checkout=ROOT.parent / "x-idl" if args.checkouts else None)))
    elif args.command == "sdk":
        print(sdk(args.output, args.tag, allow_dirty=args.allow_dirty))
    elif args.command == "tools":
        print(generator(args.output, args.platform, args.binary, args.generated, args.tag))
    else:
        print(checksums(args.directory, args.complete))


if __name__ == "__main__":
    main()
