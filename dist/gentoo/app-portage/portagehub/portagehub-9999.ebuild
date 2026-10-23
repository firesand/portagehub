# Copyright 2026 PortageHub authors
# Distributed under the terms of the GNU General Public License v2

EAPI=8

RUST_MIN_VER="1.95.0"

inherit cargo desktop git-r3 optfeature xdg

DESCRIPTION="Steam-styled graphical front end for Portage"
HOMEPAGE="https://github.com/firesand/portagehub"
EGIT_REPO_URI="https://github.com/firesand/portagehub.git"

LICENSE="GPL-2+"
# Dependent crate licenses
LICENSE+="
	Apache-2.0 BSD BSD-2 Boost-1.0 ISC MIT OFL-1.1 UbuntuFontLicense-1.0
	Unicode-3.0 ZLIB
"
SLOT="0"

RDEPEND="
	dev-libs/wayland
	media-libs/fontconfig
	media-libs/libglvnd
	sys-apps/portage
	sys-auth/polkit
	x11-libs/libX11
	x11-libs/libXcursor
	x11-libs/libXi
	x11-libs/libXrandr
	x11-libs/libxkbcommon
"

src_unpack() {
	git-r3_src_unpack
	cargo_live_src_unpack
}

src_compile() {
	# Portage strips the binaries itself.
	local -x CARGO_PROFILE_RELEASE_STRIP=false
	cargo_src_compile
}

src_test() {
	# The helper tests use a scratch root; the ones that run emerge need a
	# working Portage outside the sandbox.
	local -x SKIP_EMERGE_TESTS=1
	cargo_src_test
}

src_install() {
	dobin "$(cargo_target_dir)/portagehub"
	exeinto /usr/libexec
	doexe "$(cargo_target_dir)/portagehub-helper"
	insinto /usr/share/polkit-1/actions
	doins dist/org.portagehub.helper.policy
	domenu dist/portagehub.desktop
	dodoc README.md SECURITY.md
}

pkg_postinst() {
	xdg_pkg_postinst
	optfeature "the intended interface font" media-fonts/noto
	optfeature "the intended monospace font" media-fonts/jetbrains-mono
	elog "History and live build logs need your user in the portage group:"
	elog "  usermod -aG portage <user>"
}
