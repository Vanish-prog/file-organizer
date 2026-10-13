# File Organizer

Native Rust/GPUI file organizer using the existing `findmatch` CLI for local semantic ranking. Photo previews are enabled by default.

## Development launch (NixOS / x86_64 Linux)

```sh
./run.sh
./run.sh /path/to/photos
```

The Nix development shell supplies the Rust compiler, native GPUI libraries and the pinned `findmatch` package. Nothing is installed into your system configuration. First build downloads Cargo dependencies and compiles GPUI; subsequent launches reuse the build. Search uses an absolute Nix store path, not an existing `findmatch` on PATH.

```sh
nix develop --command cargo test --locked
nix develop --command cargo build --locked
```

## Nix package (x86_64 Linux)

```sh
nix build .#default --no-link --print-out-paths
nix run .# -- /path/to/photos
bash tests/nix-package.sh
```

`packages.x86_64-linux.default` is a sandbox-buildable Rust package using `Cargo.lock` and a fixed Cargo vendor hash. It includes a desktop entry and native GPUI runtime libraries. `findmatch` is an explicit flake dependency pinned to `Vanish-prog/search-tool` revision `5b80bb835f0f454acddb360c037e369ef879582d`, with its own upstream nixpkgs lock preserved. The package closure includes findmatch and its format-processing dependencies; an ambient findmatch installation is not needed.

The absolute findmatch executable is baked into the Rust binary and set by the package wrapper. The development shell sets the same path. Unwrapped development/test binaries permit a deliberate `FILE_ORGANIZER_FINDMATCH` runtime override; the production wrapper resets it to the pinned dependency. Native-library paths are supplied by the wrapper for dynamically loaded GPUI libraries.

Self-contained here means executable dependencies, not model data or a desktop session. A working Wayland/X11 session and graphics drivers are still required. The embedding model remains in your user cache and is never downloaded during the Nix build; first use needs the app's separate model-download permission. No NixOS or Home Manager configuration is changed by building or running this flake. To install persistently, add this flake's default package to your managed Home Manager packages.

## Safety

Work on a backed-up folder for your first session. Duplicate suggestions require human review; similar-looking photos are not necessarily duplicates. Merge keeps your chosen master and moves selected other files to the system Trash, rather than irreversibly deleting them. There is no automatic deletion.

Folder inclusion and flattening do not overwrite name collisions. These moves currently use no-overwrite hard links and require same-filesystem hard-link support; FAT/exFAT volumes and cross-filesystem nested mounts are not supported for these operations. Symbolic links are excluded from inventory and never followed during file organization. Flattening requires confirmation and removes only directories which are empty after the move. Hidden regular files are included. A filesystem operation can fail midway due to I/O errors; inspect the reported error and rescan before retrying.

Categorization uses the folder name plus the chosen reference file as a findmatch query. Semantic cosine scores are not duplicate confidence or probabilities. New/changed files require explicit processing permission; an initial model download requires separate permission. Unknown formats may be ranked by metadata only, per findmatch's format support.

## Provenance and dependencies

Application code, tests, documentation and packaging are completely AI-authored with Hermes Agent. No application license has been selected yet. Third-party code retains its own licenses; GPUI is Apache-2.0 and findmatch is MIT. Model licensing is separate.

`vendor/xattr` contains the upstream MIT/Apache-2.0 xattr 0.2.3 crate with a narrow compatibility patch: Linux/Android's removed `libc::ENOATTR` alias is replaced by `libc::ENODATA`. This is needed by GPUI 0.2.2's transitive archive dependency with the current libc crate. `Cargo.lock` pins dependencies.
