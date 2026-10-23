# PortageHub overlay

A complete overlay with `app-portage/portagehub`:

- `portagehub-9999.ebuild` builds the latest `master` from git;
- `portagehub-<version>.ebuild` builds a release tarball (crates listed in
  `CRATES`, fetched by Portage).

## Use it

```sh
sudo cp -r dist/gentoo /var/db/repos/portagehub
printf '[portagehub]\nlocation = /var/db/repos/portagehub\n' | sudo tee /etc/portage/repos.conf/portagehub.conf
sudo ebuild /var/db/repos/portagehub/app-portage/portagehub/portagehub-0.4.0.ebuild manifest
sudo emerge --ask app-portage/portagehub
```

The live ebuild needs `app-portage/portagehub **` in
`/etc/portage/package.accept_keywords`. While the GitHub repository is private,
Portage cannot download it anonymously. Until it is public, build with
`cargo build --release && sudo dist/install.sh`.

## New release

1. Bump `version` in `Cargo.toml`, then copy the newest release ebuild to the
   new version.
2. Run `cargo fetch`, then
   `python3 dist/gentoo/update-ebuild.py dist/gentoo/app-portage/portagehub/portagehub-<new>.ebuild`.
   This fills `CRATES` and the crate licenses from `Cargo.lock`.
3. Run `pkgcheck scan` in `dist/gentoo`. `MissingManifest` is expected until
   the manifest is generated from the release tarball.
