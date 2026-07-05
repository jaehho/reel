# Packaging

AUR package `reel-git` — builds the desktop app from `main` and installs the
binary, a `.desktop` launcher entry, and icons.

Files:

- `PKGBUILD` — the AUR recipe (VCS `-git` package, no release tags needed).
- `.SRCINFO` — generated metadata AUR requires (`makepkg --printsrcinfo`).
- `reel.desktop` — launcher entry, installed to `/usr/share/applications`.

## Prerequisite

The `PKGBUILD` sources from `https://github.com/jaehho/reel.git`, so that repo
must exist and be pushed before the package can build for anyone but you.

## Test the build locally (no AUR, no GitHub)

Point the source at this working tree and build:

```fish
cd /tmp; rm -rf reel-pkgtest; mkdir reel-pkgtest; cd reel-pkgtest
cp ~/projects/reel/packaging/PKGBUILD .
# temporarily build from the local repo instead of GitHub:
sed -i 's#git+https://github.com/jaehho/reel.git#git+file:///home/jaeho/projects/reel#' PKGBUILD
makepkg -f            # build; add -si to also install
```

`namcap PKGBUILD` and `namcap *.pkg.tar.zst` lint the recipe and the result.

## Publish to the AUR (first time)

1. Push the source to GitHub (`github.com/jaehho/reel`) so the `source=` URL resolves.
2. Regenerate metadata: `makepkg --printsrcinfo > .SRCINFO`.
3. Clone the (empty) AUR repo and push:

   ```fish
   git clone ssh://aur@aur.archlinux.org/reel-git.git aur-reel
   cp PKGBUILD .SRCINFO aur-reel/
   cd aur-reel; git add PKGBUILD .SRCINFO
   git commit -m "Initial import: reel-git"
   git push
   ```

   (Requires an AUR account with an SSH key registered.)

## Update (after pushing new commits to reel)

`-git` packages track `main` automatically — rebuilding pulls the latest. Only
re-touch the AUR when the recipe itself changes (deps, install layout):

```fish
makepkg --printsrcinfo > .SRCINFO   # in this dir, then copy both to the AUR clone
```

## Wire into dotfiles

Once `reel-git` is on the AUR, add one line to `dotfiles/packages/arch.txt`:

```
reel-git
```

`make sync` installs it via paru like every other package — no submodule, no
special-casing.

## Known warnings

`makepkg` reports `Package contains reference to $srcdir` on `usr/bin/reel`.
That's tauri's `generate_context!` baking `CARGO_MANIFEST_DIR` (the build path to
`src-tauri`) into the binary — a dead compile-time string, since the frontend is
embedded and nothing reads that path at runtime. It's expected for Tauri apps;
`--remap-path-prefix` in `build()` strips the remaining (rustc `file!()`) refs.

## Assumptions to verify

- **Toolchain**: `makedepends=('rustup')` matches this machine (rustup, not the
  `rust` package). For a clean-chroot build, swap to `rust`.
- **`StartupWMClass=reel`**: guessed. After first launch, confirm the window's
  app-id with `hyprctl clients | grep -i class` and correct the `.desktop` if it
  differs (Tauri may report the `dev.jaeho.reel` identifier instead).
