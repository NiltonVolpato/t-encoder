#!/usr/bin/env bash
#
# Script to check what embedded resources are being compiled into the firmware.
set -euo pipefail

MANIFEST="$(cargo locate-project --workspace 2>/dev/null | jq -r '.root')"
PROJECT_DIR="$(dirname "$MANIFEST")"
TARGET_DIR="${PROJECT_DIR}/target/xtensa-esp32s3-none-elf/release"
BIN="${TARGET_DIR}/firmware"

SKIP_BUILD=0

for arg in "$@"; do
    case "$arg" in
        -s|--skip-build|--quick)
            SKIP_BUILD=1
            ;;
        -h|--help)
            echo "Usage: $0 [OPTIONS]"
            echo
            echo "Options:"
            echo "  -s, --skip-build, --quick   Skip cargo build --release and inspect existing artifacts"
            echo "  -h, --help                  Show this help message"
            exit 0
            ;;
        *)
            echo "Unknown option: $arg" >&2
            echo "Use --help for usage." >&2
            exit 1
            ;;
    esac
done

if [ "$SKIP_BUILD" -eq 0 ]; then
    echo "==> Building release firmware in ${PROJECT_DIR}..."
    (cd "${PROJECT_DIR}" && cargo build --release)
    echo
fi

if [ ! -d "${TARGET_DIR}/build" ]; then
    echo "Error: build directory not found at ${TARGET_DIR}/build" >&2
    exit 1
fi

inspect_crate() {
    local crate_name="$1"
    local file_pattern="$2"

    local file_path
    file_path=$(ls -t ${TARGET_DIR}/build/${file_pattern} 2>/dev/null | head -n 1 || true)

    echo "================================================================================"
    echo "Crate: ${crate_name}"
    if [ -z "$file_path" ] || [ ! -f "$file_path" ]; then
        echo "  Generated file: (none found matching ${file_pattern})"
        echo
        return
    fi
    echo "  Generated file: ${file_path}"
    echo

    local fonts
    fonts=$(grep -E 'const[[:space:]]+SLINT_EMBEDDED_FONT_[A-Za-z0-9_]+[[:space:]]*:[[:space:]]*sp[[:space:]]*::[[:space:]]*BitmapFont' "$file_path" || true)

    local char_maps
    char_maps=$(grep -E 'const[[:space:]]+SLINT_EMBEDDED_CHAR_MAP_[A-Za-z0-9_]+[[:space:]]*:[[:space:]]*\[[[:space:]]*sp[[:space:]]*::[[:space:]]*CharacterMapEntry' "$file_path" || true)

    local images
    images=$(grep -E 'const[[:space:]]+SLINT_EMBEDDED_IMAGE_[A-Za-z0-9_]+[[:space:]]*:[[:space:]]*sp[[:space:]]*::[[:space:]]*StaticTextures' "$file_path" || true)

    local font_count=0
    local char_map_count=0
    local image_count=0

    [ -n "$fonts" ] && font_count=$(echo "$fonts" | wc -l | tr -d ' ')
    [ -n "$char_maps" ] && char_map_count=$(echo "$char_maps" | wc -l | tr -d ' ')
    [ -n "$images" ] && image_count=$(echo "$images" | wc -l | tr -d ' ')

    echo "  [Fonts: ${font_count}]"
    if [ -n "$fonts" ]; then
        echo "$fonts" | sed -E 's/^[[:space:]]*const[[:space:]]+([A-Za-z0-9_]+)[[:space:]]*:.*BitmapFont.*/    - \1/'
    else
        echo "    (none)"
    fi
    echo

    echo "  [Character Maps: ${char_map_count}]"
    if [ -n "$char_maps" ]; then
        echo "$char_maps" | sed -E 's/^[[:space:]]*const[[:space:]]+([A-Za-z0-9_]+)[[:space:]]*:.*CharacterMapEntry.*/    - \1/'
    else
        echo "    (none)"
    fi
    echo

    echo "  [Images / Textures: ${image_count}]"
    if [ -n "$images" ]; then
        echo "$images" | sed -E 's/^[[:space:]]*const[[:space:]]+([A-Za-z0-9_]+)[[:space:]]*:.*StaticTextures.*/    - \1/'
    else
        echo "    (none)"
    fi
    echo
}

inspect_crate "app-launcher" "app-launcher-*/out/launcher.rs"
inspect_crate "app-clock" "app-clock-*/out/clock.rs"
inspect_crate "theme" "theme-*/out/theme.rs"

echo "================================================================================"
echo "Bloaty Size Analysis"
echo "================================================================================"
if ! command -v bloaty >/dev/null 2>&1; then
    echo "Warning: bloaty is not installed or not in PATH."
elif [ ! -f "$BIN" ]; then
    echo "Warning: firmware binary not found at ${BIN}"
else
    echo "Firmware binary: ${BIN}"
    echo
    echo "--- Sections ---"
    bloaty "$BIN" -d sections
    echo
    echo "--- Top 15 Compile Units ---"
    bloaty "$BIN" -d compileunits -n 15
    echo
    echo "--- Top 15 Symbols ---"
    bloaty "$BIN" -d symbols -n 15
fi
