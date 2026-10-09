# Maintainer: John Ettedgui <git@johne.rojalabs.com>
pkgname=pkgdu
pkgver=0.1.0
pkgrel=1
pkgdesc="Real on-disk usage per installed Arch Linux package"
arch=('x86_64' 'aarch64')
url="https://github.com/John-Gee/pkgdu"
license=('GPL-3.0-or-later')
depends=('gcc-libs')
makedepends=('rust')
source=("$pkgname::git+$url")
sha256sums=('SKIP')

pkgver() {
	cd "$pkgname"
	printf '0.1.0.r%s.g%s' "$(git rev-list --count HEAD)" "$(git rev-parse --short=7 HEAD)"
}

build() {
	cd "$pkgname"
	cargo build --release --locked
}

package() {
	cd "$pkgname"
	install -Dm755 target/release/pkgdu "$pkgdir/usr/bin/pkgdu"
	install -Dm644 LICENSE "$pkgdir/usr/share/licenses/$pkgname/LICENSE"
}
