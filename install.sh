#!/bin/sh
# Installs LyricsLens into ~/.local. Nothing outside your home directory is
# touched, and no sudo is asked for.
#
#   curl -fsSL https://raw.githubusercontent.com/byjuliocesa/lyricslens/main/install.sh | sh
#   ... | sh -s -- --remove
set -eu

REPO="byjuliocesa/lyricslens"
# The API by repository id, which survives a rename of the owner or the name.
API="https://api.github.com/repositories/1338589467"
NAME="lyricslens"
SHORT="lls"
PREFIX="${XDG_DATA_HOME:-$HOME/.local/share}"
BIN="$HOME/.local/bin"
APPS="$PREFIX/applications"
ICONS="$PREFIX/icons/hicolor"
HOME_DIR="$PREFIX/$NAME"
BASH_COMPLETIONS="$PREFIX/bash-completion/completions"
FISH_COMPLETIONS="${XDG_CONFIG_HOME:-$HOME/.config}/fish/completions"
ZSHRC="${ZDOTDIR:-$HOME}/.zshrc"
# Marks the line this script adds to .zshrc, so --remove finds it again.
ZSH_MARK="# added by the lyricslens installer"

say() { printf '%s\n' "$*"; }
die() { printf 'error: %s\n' "$*" >&2; exit 1; }
step() { printf '  %-12s %s\n' "$1" "$(printf '%s' "$2" | sed "s|^$HOME/|~/|")"; }

# A progress bar when someone is watching, nothing when the output is a log.
if [ -t 2 ]; then
    CURL_PROGRESS="--progress-bar"
else
    CURL_PROGRESS="--silent --show-error"
fi

remove() {
    rm -rf "$HOME_DIR"
    rm -f "$BIN/$NAME" "$BIN/$SHORT" "$APPS/$NAME.desktop"
    rm -f "$BASH_COMPLETIONS/$NAME" "$BASH_COMPLETIONS/$SHORT"
    rm -f "$FISH_COMPLETIONS/$NAME.fish" "$FISH_COMPLETIONS/$SHORT.fish"
    if [ -f "$ZSHRC" ] && grep -qF "$ZSH_MARK" "$ZSHRC"; then
        # Written back in place rather than moved over it: a .zshrc that is a
        # symlink into a dotfiles repository stays one.
        kept=$(grep -vF "$ZSH_MARK" "$ZSHRC" || true)
        printf '%s\n' "$kept" > "$ZSHRC"
    fi
    for size in 32 64 128 256; do
        rm -f "$ICONS/${size}x${size}/apps/$NAME.png"
    done
    command -v update-desktop-database >/dev/null 2>&1 && update-desktop-database "$APPS" 2>/dev/null || true
    say "LyricsLens removed. Your settings in ~/.config/$NAME and cached lyrics"
    say "in ~/.cache/$NAME were left alone; delete them by hand if you want to."
}

if [ "${1:-}" = "--remove" ] || [ "${1:-}" = "-r" ]; then
    remove
    exit 0
fi

command -v curl >/dev/null 2>&1 || die "curl is needed"
command -v tar >/dev/null 2>&1 || die "tar is needed"

case "$(uname -m)" in
    x86_64) ;;
    *) die "only x86_64 is published; build from source with: cargo install --path ." ;;
esac

# The libraries the binary links against are not bundled: they are the ones a
# desktop already has, and bundling GTK would turn 16MB into 80MB.
# gtk4-layer-shell is not in the list because it is built into the binary:
# Ubuntu has no package for it before 25.10.
missing=""
for library in libgtk-4.so.1 libadwaita-1.so.0; do
    if ! ldconfig -p 2>/dev/null | grep -q "$library"; then
        missing="$missing $library"
    fi
done
if [ -n "$missing" ]; then
    say "These libraries are missing:$missing"
    say "On Arch:   sudo pacman -S gtk4 libadwaita"
    say "On Debian: sudo apt install libgtk-4-1 libadwaita-1-0"
    die "install them and run this again"
fi

# The binary is not static: it is built against the libraries of the machine
# that built it, and an older system cannot run it. Better to say so than to
# hand over a download that dies on its first symbol.
check_glibc() {
    needed=$1
    have=$(ldd --version 2>/dev/null | head -n 1 | grep -o '[0-9]\+\.[0-9]\+' | head -n 1)
    [ -n "$have" ] || return 0

    oldest=$(printf '%s\n%s\n' "$needed" "$have" | sort -V | head -n 1)
    [ "$oldest" = "$needed" ] && return 0

    say "This build needs glibc $needed and this system has $have."
    say "Install the .deb or .rpm from the release instead, or build from"
    say "source with: cargo install --git https://github.com/$REPO"
    die "not installed"
}

TAG="${LYRICSLENS_VERSION:-}"
if [ -z "$TAG" ]; then
    TAG=$(curl -fsSL "$API/releases/latest" \
        | sed -n 's/.*"tag_name": *"\([^"]*\)".*/\1/p' | head -n 1)
fi
[ -n "$TAG" ] || die "could not find the latest release"
say "Release: $TAG"

ARCHIVE="$NAME-$TAG-x86_64-linux.tar.gz"
BASE="https://github.com/$REPO/releases/download/$TAG"

needed=$(curl -fsSL "$BASE/MINIMUM_GLIBC" 2>/dev/null | tr -d '[:space:]')
[ -n "$needed" ] && check_glibc "$needed"

WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT INT TERM

say "Downloading $BASE/$ARCHIVE"
# shellcheck disable=SC2086 # two flags in one variable, split on purpose
curl -fL $CURL_PROGRESS "$BASE/$ARCHIVE" -o "$WORK/$ARCHIVE" || die "could not download $ARCHIVE"
say "Downloaded $(du -h "$WORK/$ARCHIVE" | cut -f1)."

# A download nobody checks is a download nobody can trust.
if curl -fsSL "$BASE/SHA256SUMS" -o "$WORK/SHA256SUMS" 2>/dev/null; then
    if command -v sha256sum >/dev/null 2>&1; then
        (cd "$WORK" && sha256sum -c SHA256SUMS --ignore-missing >/dev/null) \
            || die "the checksum does not match; the download was not installed"
        say "Checksum verified (SHA256)."
    fi
else
    say "warning: this release publishes no SHA256SUMS; skipping the check"
fi

# The archive carries a top directory named after the release; the paths below
# are written without it.
tar -xzf "$WORK/$ARCHIVE" -C "$WORK" --strip-components=1

say "Installing:"
mkdir -p "$HOME_DIR" "$BIN" "$APPS"
install -Dm755 "$WORK/$NAME" "$HOME_DIR/$NAME"
step program "$HOME_DIR/$NAME"

# A launcher rather than a symlink: the .desktop file and the shell both point
# here, and the real binary can move without either noticing.
cat > "$BIN/$NAME" <<LAUNCHER
#!/bin/sh
exec "$HOME_DIR/$NAME" "\$@"
LAUNCHER
chmod 755 "$BIN/$NAME"
step command "$BIN/$NAME"

# The same launcher under a name worth typing.
ln -sf "$NAME" "$BIN/$SHORT"
step "short name" "$BIN/$SHORT"

for size in 32 64 128 256; do
    if [ -f "$WORK/icons/$size.png" ]; then
        install -Dm644 "$WORK/icons/$size.png" "$ICONS/${size}x${size}/apps/$NAME.png"
    fi
done
step icons "$ICONS"

cat > "$APPS/$NAME.desktop" <<DESKTOP
[Desktop Entry]
Type=Application
Name=LyricsLens
GenericName=Lyrics overlay
Comment=Synced lyrics on top of any application
Exec=$BIN/$NAME
Icon=$NAME
Terminal=false
Categories=AudioVideo;Audio;Player;
StartupNotify=false
DESKTOP
step "menu entry" "$APPS/$NAME.desktop"

# Releases before 0.1.36 carry no completions.
if [ -d "$WORK/completions" ]; then
    # Both shells load a completion by the name of the command typed, so each
    # name gets its own file.
    install -Dm644 "$WORK/completions/$NAME.bash" "$BASH_COMPLETIONS/$NAME"
    ln -sf "$NAME" "$BASH_COMPLETIONS/$SHORT"
    install -Dm644 "$WORK/completions/$NAME.fish" "$FISH_COMPLETIONS/$NAME.fish"
    ln -sf "$NAME.fish" "$FISH_COMPLETIONS/$SHORT.fish"

    # zsh has no folder of its own under the home directory that it reads by
    # itself, so .zshrc is told where to look. compdef exists only once
    # compinit has run; before that, the folder on fpath is enough.
    install -Dm644 "$WORK/completions/_$NAME" "$HOME_DIR/completions/_$NAME"
    if [ -f "$ZSHRC" ] && ! grep -qF "$ZSH_MARK" "$ZSHRC"; then
        printf '%s\n' "fpath+=(\"$HOME_DIR/completions\"); (( \$+functions[compdef] )) && autoload -Uz _$NAME && compdef _$NAME $NAME $SHORT $ZSH_MARK" >> "$ZSHRC"
    fi
    step completions "bash, zsh, fish (open a new terminal to use them)"
fi

command -v update-desktop-database >/dev/null 2>&1 && update-desktop-database "$APPS" 2>/dev/null || true

say ""
say "LyricsLens $TAG installed."
case ":$PATH:" in
    *":$BIN:"*) say "Run it with: $NAME" ;;
    *) say "Run it with: $BIN/$NAME"
       say "($BIN is not on your PATH; add it to use the short name.)" ;;
esac
say "Short name: $SHORT"
say "Preferences: $NAME --settings"
