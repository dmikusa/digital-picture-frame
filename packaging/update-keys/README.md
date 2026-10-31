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

Create an Ed25519 SSH key in Bitwarden (or with `ssh-keygen -t ed25519`) and
copy its public-key line into this directory. The matching private key stays in
Bitwarden and is copied into the `release-signing` GitHub environment as the
`UPDATE_SIGNING_KEY` secret. The private key must be an OpenSSH key and must be
usable without an interactive passphrase in CI.

Never commit a private key.
