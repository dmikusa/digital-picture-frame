# Update signing public keys

Put one public key per file here. The filename is the key ID:

```text
packaging/update-keys/2026-10.pub
```

Each file contains one OpenSSH `ssh-ed25519` public-key line:

```text
ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAA... 2026-10
```

`build.rs` parses every `*.pub` file and compiles the raw 32-byte keys into
`photo-frame-update`. Files in this directory are public; never put a private key
here.

Generate a PEM private key, then derive the matching OpenSSH public key:

```bash
openssl genpkey -algorithm ED25519 -out update-signing-2026-10.pem
chmod 600 update-signing-2026-10.pem

scripts/openssh-public-key.sh update-signing-2026-10.pem 2026-10 \
  > packaging/update-keys/2026-10.pub
```

Keep `update-signing-2026-10.pem` out of the repository. The signing workflow
uses its contents as the `UPDATE_SIGNING_KEY` environment secret.
