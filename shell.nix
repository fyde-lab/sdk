{ pkgs ? import <nixpkgs> {
    overlays = [
      (import (builtins.fetchTarball
        "https://github.com/oxalica/rust-overlay/archive/master.tar.gz"))
    ];
  }
}:

let
  rustToolchain = pkgs.rust-bin.stable."1.97.1".default.override {
    extensions = [ "rust-src" "clippy" "rustfmt" "rust-analyzer" ];
  };
in
pkgs.mkShell {
  packages = with pkgs; [
    rustToolchain

    # required by build.rs to compile ../api-protos/*.proto via tonic-prost-build
    protobuf

    # required to build wreq's vendored, patched BoringSSL (btls-sys), used by the
    # scrapers domain's fyde.http (see src/domains/scrapers/host/http.rs) for a real
    # Chrome TLS/HTTP2 fingerprint.
    cmake
    perl
    go
    # Sets up LIBCLANG_PATH/BINDGEN_EXTRA_CLANG_ARGS correctly for bindgen.
    rustPlatform.bindgenHook
  ];
}
