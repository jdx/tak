# Contributing

Use the mise tasks documented in `AGENTS.md` for development and validation.

## mbx build cache

`mise install` installs [mbx](https://mr-boxington.jdx.dev) 1.1 and activates
its transparent Cargo shim. The normal `mise run build`, `mise run test`, and
`mise run lint` workflows therefore use the cache while invoking Cargo
normally. To bypass mbx without skipping or weakening a check, prefix the
equivalent Cargo command with `MBX_DISABLE=1`:

```sh
MBX_DISABLE=1 cargo build
MBX_DISABLE=1 cargo test --all-targets
MBX_DISABLE=1 cargo clippy --all-targets -- -D warnings
```

If bypassed Cargo succeeds where the shim fails, or mbx introduces a papercut, please start a
[mr-boxington Discussion](https://github.com/jdx/mr-boxington/discussions).
Include the repository and commit, operating system, `mbx --version`,
`mbx doctor`, and both commands and their output.
