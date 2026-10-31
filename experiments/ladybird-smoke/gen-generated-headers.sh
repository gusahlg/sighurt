#!/usr/bin/env bash
# Builds the generated C++ headers that nixpkgs' `ladybird` doesn't install (LibWeb/CSS/Units.h,
# DescriptorID.h, LibJS/Bytecode/OpCodes.h, NamedCharacterReferences.h, ...): configures the exact
# source nixpkgs built (read-only copy, ICU pin patched as nixpkgs does) and builds only those
# targets with ninja, which builds just the code generators they need. Run inside shell.nix:
#
#   tools/heavy nix-shell experiments/ladybird-smoke/shell.nix --run \
#     "experiments/ladybird-smoke/gen-generated-headers.sh CACHE_DIR"
#
# CACHE_DIR/build/Libraries then holds LibWeb/CSS/*.h, LibJS/Bytecode/*.h, ...
set -euo pipefail
cache=$(realpath -m "${1:?usage: gen-generated-headers.sh CACHE_DIR}")
mkdir -p "$cache"
if [[ ! -d $cache/src ]]; then
  cp -r "$LADYBIRD_SRC" "$cache/src"
  chmod -R u+w "$cache/src"
  perl -0pi -e 's/find_package\(ICU 78\.[0-9]+ EXACT REQUIRED COMPONENTS data i18n uc\)/find_package(ICU 78.3 EXACT REQUIRED COMPONENTS data i18n uc)/' \
    "$cache/src/Meta/CMake/check_for_dependencies.cmake"
fi
mkdir -p "$cache/build/Caches/HSTSPreload"
cmake -S "$cache/src" -B "$cache/build" -G Ninja -DCMAKE_BUILD_TYPE=Release \
  -DENABLE_LTO_FOR_RELEASE=FALSE -DENABLE_NETWORK_DOWNLOADS=OFF \
  -DICU_ROOT="$(dirname "$(dirname "$(pkg-config --variable=includedir icu-uc)")")" \
  -DCMAKE_INSTALL_LIBEXECDIR=libexec -DLADYBIRD_CACHE_DIR="$cache/build/Caches" > "$cache/configure.log" 2>&1
# Generated headers of the libraries (Bindings are installed; the Rust-made ones come from
# gen-rust-headers.sh; BuildInformation.h needs git and nothing here includes it).
targets=(
  Libraries/LibJS/Bytecode/OpCodes.h Libraries/LibJS/Bytecode/Op.h
  Libraries/LibWeb/ARIA/AriaRoles.h
  Libraries/LibWeb/CSS/DescriptorID.h Libraries/LibWeb/CSS/Enums.h
  Libraries/LibWeb/CSS/GeneratedCSSNumericFactoryMethods.h Libraries/LibWeb/CSS/GeneratedCSSStyleProperties.h
  Libraries/LibWeb/CSS/Keyword.h Libraries/LibWeb/CSS/MediaFeatureID.h Libraries/LibWeb/CSS/PropertyID.h
  Libraries/LibWeb/CSS/PseudoClass.h Libraries/LibWeb/CSS/PseudoElement.h
  Libraries/LibWeb/CSS/TransformFunctions.h Libraries/LibWeb/CSS/Units.h
  Libraries/LibWeb/HTML/Parser/NamedCharacterReferences.h
)
ninja -C "$cache/build" -j"${SIG_JOBS:-2}" "${targets[@]}"
