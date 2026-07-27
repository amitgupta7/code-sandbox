#!/usr/bin/env bash
#
# Fetch/build the wasm runtime modules the sandbox needs:
#   runtimes/python.wasm  - CPython compiled to wasm32-wasi (prebuilt)
#   runtimes/qjs.wasm      - QuickJS compiled to wasm32-wasi (built from source)
#
# Idempotent: skips work whose output already exists. Re-run with FORCE=1 to
# rebuild everything.
set -euo pipefail

cd "$(dirname "$0")/.."
ROOT="$(pwd)"
mkdir -p runtimes .toolchain .src

# --- Config ---------------------------------------------------------------
PY_WASM_URL="https://github.com/vmware-labs/webassembly-language-runtimes/releases/download/python%2F3.12.0%2B20231211-040d5a6/python-3.12.0.wasm"

# Pick the wasi-sdk asset for this host.
uname_s="$(uname -s)"; uname_m="$(uname -m)"
case "$uname_s-$uname_m" in
  Darwin-arm64)  WASI_SDK_ASSET="wasi-sdk-24.0-arm64-macos" ;;
  Darwin-x86_64) WASI_SDK_ASSET="wasi-sdk-24.0-x86_64-macos" ;;
  Linux-x86_64)  WASI_SDK_ASSET="wasi-sdk-24.0-x86_64-linux" ;;
  Linux-aarch64) WASI_SDK_ASSET="wasi-sdk-24.0-arm64-linux" ;;
  *) echo "Unsupported host: $uname_s-$uname_m" >&2; exit 1 ;;
esac
WASI_SDK_URL="https://github.com/WebAssembly/wasi-sdk/releases/download/wasi-sdk-24/${WASI_SDK_ASSET}.tar.gz"

# --- Python ---------------------------------------------------------------
if [[ "${FORCE:-0}" == "1" || ! -f runtimes/python.wasm ]]; then
  echo ">> Fetching CPython WASI build..."
  curl -sSL -o runtimes/python.wasm "$PY_WASM_URL"
else
  echo ">> runtimes/python.wasm exists, skipping."
fi

# --- QuickJS --------------------------------------------------------------
if [[ "${FORCE:-0}" == "1" || ! -f runtimes/qjs.wasm ]]; then
  echo ">> Ensuring wasi-sdk..."
  if [[ ! -d ".toolchain/$WASI_SDK_ASSET" ]]; then
    curl -sSL -o .toolchain/wasi-sdk.tar.gz "$WASI_SDK_URL"
    tar -xzf .toolchain/wasi-sdk.tar.gz -C .toolchain/
  fi
  WSDK_ABS="$ROOT/.toolchain/$WASI_SDK_ASSET"

  echo ">> Ensuring QuickJS source..."
  if [[ ! -d ".src/quickjs/.git" ]]; then
    rm -rf .src/quickjs
    git clone --depth 1 https://github.com/quickjs-ng/quickjs.git .src/quickjs
  fi

  echo ">> Building qjs.wasm..."
  cmake -B .src/quickjs/build-wasi -S .src/quickjs \
    -DCMAKE_TOOLCHAIN_FILE="$WSDK_ABS/share/cmake/wasi-sdk.cmake" \
    -DCMAKE_BUILD_TYPE=Release \
    -DBUILD_SHARED_LIBS=OFF >/dev/null
  cmake --build .src/quickjs/build-wasi --target qjs_exe -j"$(getconf _NPROCESSORS_ONLN)"
  cp "$(find .src/quickjs/build-wasi -maxdepth 1 -name qjs -type f)" runtimes/qjs.wasm
else
  echo ">> runtimes/qjs.wasm exists, skipping."
fi

echo ">> Done."
ls -la runtimes/
