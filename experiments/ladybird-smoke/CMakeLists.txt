cmake_minimum_required(VERSION 3.25)
project(ladybird_smoke CXX)
set(CMAKE_CXX_STANDARD 23)
set(CMAKE_CXX_STANDARD_REQUIRED ON)

# nixpkgs' install tree has no Export.h: Ladybird generates them with generate_export_header at build
# time and doesn't install them. They only mark symbols (-fvisibility=hidden in the libraries), so a
# consumer needs the macros defined as default visibility.
set(shim "${CMAKE_BINARY_DIR}/shim")
foreach(pair Core:CORE Database:DATABASE DevTools:DEVTOOLS DNS:DNS GC:GC JS:JS Media:MEDIA Regex:REGEX
        TextCodec:TEXTCODEC Wasm:WASM Web:WEB WebView:WEBVIEW XML:XML)
    string(REPLACE ":" ";" parts "${pair}")
    list(GET parts 0 lib)
    list(GET parts 1 macro)
    file(WRITE "${shim}/Lib${lib}/Export.h"
        "#pragma once\n#ifndef ${macro}_API\n#define ${macro}_API __attribute__((visibility(\"default\")))\n#endif\n#ifndef ${macro}_NO_EXPORT\n#define ${macro}_NO_EXPORT __attribute__((visibility(\"hidden\")))\n#endif\n")
endforeach()

# Generated IPC endpoint headers are installed, but beside their library rather than where the
# including headers look for them (<ImageDecoder/...>, <LibWeb/Worker/...>, ...): link them in.
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
add_executable(smoke smoke.cpp)
# Ladybird sets these globally (Meta/CMake/*.cmake) and Lagom's exported targets do not carry them.
# Some change class layouts (USE_VULKAN_DMABUF_IMAGES adds a member to Gfx::SharedImageBuffer), so
# a missing one is a silent ABI mismatch with the nixpkgs libraries: keep this list equal to the
# -D/-f flags of `ninja -t commands` for LibWebView in a configured tree of the same source.
target_compile_definitions(smoke PRIVATE USE_VULKAN=1 USE_VULKAN_DMABUF_IMAGES=1 USE_FONTCONFIG=1
    HAVE_WASM_COMPILER_SERVICE=1 ENABLE_COMPILETIME_FORMAT_CHECK NDEBUG _FILE_OFFSET_BITS=64)
target_compile_options(smoke PRIVATE -fno-exceptions -fsigned-char -fno-semantic-interposition
    -ffp-contract=off -fstrict-flex-arrays=2 -ftrivial-auto-var-init=zero -fvisibility-inlines-hidden)
target_include_directories(smoke PRIVATE "${shim}" $ENV{LADYBIRD_RUST_HEADERS})
# Headers of the source tree that the install omits (RequestServer's enums), and ANGLE's GLES headers.
target_include_directories(smoke AFTER PRIVATE "$ENV{LADYBIRD_SRC}/Services" "$ENV{LADYBIRD_ANGLE}/include" "$ENV{LADYBIRD_GEN}")
target_link_libraries(smoke PRIVATE Lagom::WebView Lagom::Web Lagom::Gfx Lagom::IPC Lagom::URL Lagom::Requests Lagom::JS Lagom::Main Lagom::FileSystem Lagom::Core Lagom::Unicode Lagom::AK)
