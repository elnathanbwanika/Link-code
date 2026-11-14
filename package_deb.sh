#!/usr/bin/env bash
# package_deb.sh — build a .deb installer for link-code
set -euo pipefail

BINARY=/workspaces/Link-code/link-code/target/release/link-code
VERSION=1.0.20
ARCH=amd64
PKGNAME=link-code
PKGDIR=/tmp/link-code-deb

# -- Verify the binary exists --------------------------------------------------
if [[ ! -f "$BINARY" ]]; then
  echo "ERROR: release binary not found at $BINARY"
  exit 1
fi

# -- Create directory tree -----------------------------------------------------
rm -rf "$PKGDIR"
mkdir -p "$PKGDIR"/DEBIAN
mkdir -p "$PKGDIR"/usr/bin
mkdir -p "$PKGDIR"/usr/lib/link-code
mkdir -p "$PKGDIR"/usr/share/applications
mkdir -p "$PKGDIR"/usr/share/icons/hicolor/scalable/apps
mkdir -p "$PKGDIR"/usr/share/metainfo

# -- Copy the actual binary to /usr/lib/link-code/ ----------------------------
install -m 755 "$BINARY" "$PKGDIR/usr/lib/link-code/link-code"

# -- Wrapper launcher script at /usr/bin/link-code ----------------------------
# This ensures the app can be found by name and captures any crash output.
cat > "$PKGDIR/usr/bin/link-code" <<'WRAPPER'
#!/bin/sh
mkdir -p "$HOME/.local/share/link-code"
exec /usr/lib/link-code/link-code "$@" 2>>"$HOME/.local/share/link-code/link-code.log"
WRAPPER
chmod 755 "$PKGDIR/usr/bin/link-code"

# -- .desktop file (freedesktop app launcher) ----------------------------------
# Only ONE .desktop file = only ONE icon in the app drawer.
cat > "$PKGDIR/usr/share/applications/link-code.desktop" <<'DESKTOP'
[Desktop Entry]
Name=Link Code
GenericName=AI Coding IDE
Comment=Native AI-powered coding IDE for Ubuntu Linux
Exec=link-code %f
Icon=link-code
Terminal=false
Type=Application
Categories=Development;IDE;TextEditor;
MimeType=inode/directory;
Keywords=AI;code;editor;IDE;
StartupNotify=true
StartupWMClass=org.linkecosystem.LinkCode
DESKTOP

# -- SVG icon ------------------------------------------------------------------
cat > "$PKGDIR/usr/share/icons/hicolor/scalable/apps/link-code.svg" <<'SVG'
<?xml version="1.0" encoding="UTF-8"?>
<svg xmlns="http://www.w3.org/2000/svg" width="64" height="64" viewBox="0 0 64 64">
  <rect width="64" height="64" rx="14" fill="#000000"/>
  <path d="M32 12 L44 34 L38 34 L38 52 L26 52 L26 34 L20 34 Z" fill="#ff6600"/>
  <rect x="18" y="20" width="28" height="5" rx="2.5" fill="#ff6600" opacity="0.55"/>
</svg>
SVG

# -- Also install icon under the app-id name (for GNOME Software / portals) ----
cp "$PKGDIR/usr/share/icons/hicolor/scalable/apps/link-code.svg" \
   "$PKGDIR/usr/share/icons/hicolor/scalable/apps/org.linkecosystem.LinkCode.svg"
# NOTE: do NOT copy the .desktop file — one .desktop = one launcher icon

# -- AppStream metainfo --------------------------------------------------------
cat > "$PKGDIR/usr/share/metainfo/org.linkecosystem.LinkCode.metainfo.xml" <<'META'
<?xml version="1.0" encoding="UTF-8"?>
<component type="desktop-application">
  <id>org.linkecosystem.LinkCode</id>
  <name>Link Code</name>
  <summary>Native AI coding IDE for Ubuntu Linux</summary>
  <description><p>Link Code is a GTK4 + libadwaita native IDE that pairs you with AI models for fast, agentic coding.</p></description>
  <url type="homepage">https://github.com/tracerdasu43/Link-code</url>
  <releases>
    <release version="1.0.20" date="2026-11-14"/>
  </releases>
  <content_rating type="oars-1.1"/>
</component>
META

# -- DEBIAN control file -------------------------------------------------------
chmod 755 "$PKGDIR"
chmod 755 "$PKGDIR/DEBIAN"
INSTALLED_SIZE=$(du -sk "$PKGDIR" | cut -f1)
cat > "$PKGDIR/DEBIAN/control" <<CONTROL
Package: $PKGNAME
Version: $VERSION
Architecture: $ARCH
Maintainer: Link Ecosystem <phadisontracer@gmail.com>
Installed-Size: $INSTALLED_SIZE
Depends: libc6 (>= 2.34), libgtk-4-1 (>= 4.0), libadwaita-1-0 (>= 1.0), libsecret-1-0, libglib2.0-0t64 | libglib2.0-0, libgcc-s1
Section: devel
Priority: optional
Homepage: https://github.com/tracerdasu43/Link-code
Description: Link Code — native AI coding IDE for Ubuntu Linux
 Link Code is a GTK4 + libadwaita native IDE that connects to any
 OpenAI-compatible endpoint (plus Anthropic, Gemini and DeepSeek) and acts
 as an agentic coding assistant: reading, editing, searching, and running
 code inside a sandboxed workspace. Model endpoints are user-configured;
 no vendor accounts or presets are bundled.
CONTROL

# -- post-install script: update icon cache ------------------------------------
cat > "$PKGDIR/DEBIAN/postinst" <<'POSTINST'
#!/bin/sh
set -e
if command -v gtk-update-icon-cache >/dev/null 2>&1; then
  gtk-update-icon-cache -f -t /usr/share/icons/hicolor || true
fi
if command -v update-desktop-database >/dev/null 2>&1; then
  update-desktop-database -q /usr/share/applications || true
fi
POSTINST
chmod 755 "$PKGDIR/DEBIAN/postinst"

# -- Ensure proper permissions for all files and directories -------------------
find "$PKGDIR" -type d -exec chmod 755 {} +
find "$PKGDIR" -type f -exec chmod 644 {} +
chmod 755 "$PKGDIR/usr/bin/link-code"
chmod 755 "$PKGDIR/usr/lib/link-code/link-code"
chmod 755 "$PKGDIR/DEBIAN/postinst"

# -- Build the .deb ------------------------------------------------------------
# Keep the workspace clean: the installer is built outside the repo and is
# published to the GitHub release, not kept in the source tree.
OUT="/tmp/opencode/${PKGNAME}_${VERSION}_${ARCH}.deb"
dpkg-deb --build --root-owner-group "$PKGDIR" "$OUT"
echo ""
echo "✅  Package built: $OUT"
echo "   Install with:  sudo dpkg -i $OUT"
echo "   Then run:      link-code"
echo "   Crash log:     ~/.local/share/link-code/link-code.log"
