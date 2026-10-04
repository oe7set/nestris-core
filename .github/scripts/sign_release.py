# /// script
# requires-python = ">=3.11"
# dependencies = ["cryptography>=43"]
# ///
"""Release signing for the Retroverse apps (same file in every repository).

The release workflow signs ``SHA256SUMS.txt`` with an Ed25519 key; the apps'
updaters verify the signature with the public key compiled into them before
they trust any checksum (see nestris-ltm/docs/UPDATES.md).

    python sign_release.py sign dist/SHA256SUMS.txt     # key from $RELEASE_SIGNING_KEY
    python sign_release.py verify dist/SHA256SUMS.txt <public-key-base64>
    python sign_release.py keygen <dir>                  # once, by the maintainer

Keys are raw 32-byte Ed25519 keys in base64. The signature file
(``<file>.sig``) holds one line: the base64 signature of the file's bytes.
"""

from __future__ import annotations

import base64
import os
import sys
from pathlib import Path

from cryptography.exceptions import InvalidSignature
from cryptography.hazmat.primitives import serialization
from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey, Ed25519PublicKey


def _raw_public(key: Ed25519PrivateKey) -> str:
    raw = key.public_key().public_bytes(serialization.Encoding.Raw, serialization.PublicFormat.Raw)
    return base64.b64encode(raw).decode()


def sign(path: Path) -> int:
    secret = os.environ.get("RELEASE_SIGNING_KEY", "").strip()
    if not secret:
        print("RELEASE_SIGNING_KEY is not set: no unsigned releases", file=sys.stderr)
        return 1
    key = Ed25519PrivateKey.from_private_bytes(base64.b64decode(secret))
    signature = base64.b64encode(key.sign(path.read_bytes())).decode()
    sig_path = path.with_name(path.name + ".sig")
    sig_path.write_text(signature + "\n", encoding="ascii")
    print(f"signed {path.name} -> {sig_path.name} (public key {_raw_public(key)})")
    return 0


def verify(path: Path, public_b64: str) -> int:
    public = Ed25519PublicKey.from_public_bytes(base64.b64decode(public_b64))
    sig_text = path.with_name(path.name + ".sig").read_text(encoding="ascii")
    signature = base64.b64decode(sig_text.strip())
    try:
        public.verify(signature, path.read_bytes())
    except InvalidSignature:
        print("INVALID signature", file=sys.stderr)
        return 1
    print("signature OK")
    return 0


def keygen(directory: Path) -> int:
    directory.mkdir(parents=True, exist_ok=True)
    private_file = directory / "release-signing-key.private.txt"
    if private_file.exists():
        print(f"{private_file} exists: not overwriting an existing key", file=sys.stderr)
        return 1
    key = Ed25519PrivateKey.generate()
    raw = key.private_bytes(
        serialization.Encoding.Raw, serialization.PrivateFormat.Raw, serialization.NoEncryption()
    )
    private_file.write_text(base64.b64encode(raw).decode() + "\n", encoding="ascii")
    public_file = directory / "release-signing-key.public.txt"
    public_file.write_text(_raw_public(key) + "\n", encoding="ascii")
    print(f"private key: {private_file}")
    print("  -> GitHub secret RELEASE_SIGNING_KEY; keep a backup, never commit")
    print(f"public key:  {_raw_public(key)}")
    return 0


def main(argv: list[str]) -> int:
    if len(argv) >= 2 and argv[0] == "sign":
        return sign(Path(argv[1]))
    if len(argv) >= 3 and argv[0] == "verify":
        return verify(Path(argv[1]), argv[2])
    if len(argv) >= 2 and argv[0] == "keygen":
        return keygen(Path(argv[1]))
    print(__doc__)
    return 2


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
