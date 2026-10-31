# libsighurt-ladybird-shim.so: Sighurt's C ABI over nixpkgs' Ladybird (docs/rewrite/LADYBIRD.md).
# Built by nix/build-ladybird-shim.sh inside experiments/ladybird-smoke/shell.nix, which exports
# LADYBIRD (the installed package), LADYBIRD_SRC, LADYBIRD_ANGLE, and the script exports
# LADYBIRD_RUST_HEADERS and LADYBIRD_GEN (the headers nixpkgs does not install, see the doc).
cmake_minimum_required(VERSION 3.25)
project(sighurt_ladybird_shim CXX)
set(CMAKE_CXX_STANDARD 23)
set(CMAKE_CXX_STANDARD_REQUIRED ON)
set(CMAKE_POSITION_INDEPENDENT_CODE ON)

# nixpkgs' install tree has no Export.h (generate_export_header output): default visibility.
set(shim "${CMAKE_BINARY_DIR}/shim")
foreach(pair Core:CORE Database:DATABASE DevTools:DEVTOOLS DNS:DNS GC:GC JS:JS Media:MEDIA Regex:REGEX
        TextCodec:TEXTCODEC Wasm:WASM Web:WEB WebView:WEBVIEW XML:XML)
    string(REPLACE ":" ";" parts "${pair}")
    list(GET parts 0 lib)
    list(GET parts 1 macro)
    file(WRITE "${shim}/Lib${lib}/Export.h"
        "#pragma once\n#ifndef ${macro}_API\n#define ${macro}_API __attribute__((visibility(\"default\")))\n#endif\n#ifndef ${macro}_NO_EXPORT\n#define ${macro}_NO_EXPORT __attribute__((visibility(\"hidden\")))\n#endif\n")
endforeach()

# Endpoint headers installed beside their library but included from another path.
set(lb "$ENV{LADYBIRD}/include")
file(MAKE_DIRECTORY "${shim}/ImageDecoder" "${shim}/LibWeb/Worker" "${shim}/LibMedia/VideoPresentation" "${shim}/WasmCompiler")
foreach(m "ImageDecoder|Libraries/LibImageDecoderClient|ImageDecoderClientEndpoint.h"
          "ImageDecoder|Libraries/LibImageDecoderClient|ImageDecoderServerEndpoint.h"
          "LibWeb/Worker|Libraries/LibWeb|WebWorkerClientEndpoint.h"
          "LibWeb/Worker|Libraries/LibWeb|WebWorkerServerEndpoint.h"
          "LibMedia/VideoPresentation|Libraries/LibMedia|VideoPresentationClientEndpoint.h"
          "LibMedia/VideoPresentation|Libraries/LibMedia|VideoPresentationServerEndpoint.h"
          "WasmCompiler|Libraries/LibWasmCompilerClient|WasmCompilerClientEndpoint.h"
          "WasmCompiler|Libraries/LibWasmCompilerClient|WasmCompilerServerEndpoint.h")
    string(REPLACE "|" ";" parts "${m}")
    list(GET parts 0 dest)
    list(GET parts 1 from)
    list(GET parts 2 name)
    file(CREATE_LINK "${lb}/${from}/${name}" "${shim}/${dest}/${name}" SYMBOLIC)
endforeach()

find_package(OpenSSL REQUIRED)
find_package(Vulkan REQUIRED)
find_package(VulkanHeaders CONFIG REQUIRED)
find_package(Lagom CONFIG REQUIRED PATHS $ENV{LADYBIRD}/share/Lagom NO_DEFAULT_PATH)

add_library(sighurt-ladybird-shim SHARED shim.cpp loop.cpp)
target_include_directories(sighurt-ladybird-shim PRIVATE "${shim}" $ENV{LADYBIRD_RUST_HEADERS})
target_include_directories(sighurt-ladybird-shim AFTER PRIVATE "$ENV{LADYBIRD_SRC}/Services"
    "$ENV{LADYBIRD_ANGLE}/include" "$ENV{LADYBIRD_GEN}")
# Ladybird's global compile definitions and flags (not exported by Lagom's targets; some change
# class layouts). Keep equal to `ninja -t commands` for LibWebView of a configured tree of the same
# source: nix/build-ladybird-shim.sh checks this against the source's own flags.
target_compile_definitions(sighurt-ladybird-shim PRIVATE USE_VULKAN=1 USE_VULKAN_DMABUF_IMAGES=1
    USE_FONTCONFIG=1 HAVE_WASM_COMPILER_SERVICE=1 ENABLE_COMPILETIME_FORMAT_CHECK NDEBUG
    _FILE_OFFSET_BITS=64 "SIGHURT_LADYBIRD_PREFIX=\"$ENV{LADYBIRD}\"")
target_compile_options(sighurt-ladybird-shim PRIVATE -fno-exceptions -fsigned-char
    -fno-semantic-interposition -ffp-contract=off -fstrict-flex-arrays=2 -ftrivial-auto-var-init=zero
    -fvisibility=hidden -fvisibility-inlines-hidden -Wall -Wno-unused-parameter)
target_link_libraries(sighurt-ladybird-shim PRIVATE Lagom::WebView Lagom::Web Lagom::Gfx Lagom::IPC
    Lagom::URL Lagom::Requests Lagom::JS Lagom::FileSystem Lagom::Core Lagom::Unicode Lagom::AK)
set_target_properties(sighurt-ladybird-shim PROPERTIES
    BUILD_RPATH "$ENV{LADYBIRD}/lib" INSTALL_RPATH "$ENV{LADYBIRD}/lib" BUILD_WITH_INSTALL_RPATH ON)
install(TARGETS sighurt-ladybird-shim LIBRARY DESTINATION lib)
