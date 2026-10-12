pkgname=bookos-system-monitor
pkgver=0.1.0
pkgrel=1
pkgdesc="Monitor del Sistema de BookOS: CPU, GPU, RAM, disco, red, batería y procesos"
arch=('x86_64')
url="https://github.com/Evelynx08/system-monitor"
license=('MIT')
depends=('webkit2gtk-4.1' 'gtk3' 'libsoup3' 'hwdata')
makedepends=('rust' 'cargo' 'pkgconf' 'base-devel')
source=()
options=('!strip' '!debug')

build() {
  cd "$startdir/src-tauri"
  cargo build --release --locked
}

package() {
  install -Dm755 "$startdir/src-tauri/target/release/bookos-system-monitor" \
    "$pkgdir/usr/bin/bookos-system-monitor"

  install -Dm644 /dev/stdin "$pkgdir/usr/share/applications/bookos-system-monitor.desktop" <<EOF
[Desktop Entry]
Name=System Monitor
Name[es]=Monitor del sistema
Comment=CPU, GPU, memoria, disco, red, batería y procesos
Exec=bookos-system-monitor
Icon=bookos-system-monitor
Type=Application
Categories=System;Monitor;
StartupNotify=true
StartupWMClass=bookos-system-monitor
EOF

  install -Dm644 "$startdir/src-tauri/icons/icon.png" \
    "$pkgdir/usr/share/icons/hicolor/512x512/apps/bookos-system-monitor.png"
  install -Dm644 "$startdir/src-tauri/icons/icon.svg" \
    "$pkgdir/usr/share/icons/hicolor/scalable/apps/bookos-system-monitor.svg"
}
