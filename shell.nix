{ pkgs ? import <nixpkgs> {
    overlays = [
      (import (builtins.fetchTarball
        "https://github.com/oxalica/rust-overlay/archive/master.tar.gz"))
    ];
  }
}:

let
  # wreq/wreq-util (`domains/scrapers/host/http.rs`) require rustc >=1.98.
  rustToolchain = pkgs.rust-bin.stable."1.98.1".default.override {
    extensions = [ "rust-src" "clippy" "rustfmt" "rust-analyzer" ];
  };
in
pkgs.mkShell {
  packages = with pkgs; [
    rustToolchain
    sccache

    # required by build.rs to compile ../api-protos/*.proto via tonic-prost-build
    protobuf

    # required to build wreq's vendored, patched BoringSSL (btls-sys), used by the
    # scrapers domain's fyde.http (see src/domains/scrapers/host/http.rs) for a real
    # Chrome TLS/HTTP2 fingerprint.
    cmake
    perl
    go
    clang
    # Sets up LIBCLANG_PATH/BINDGEN_EXTRA_CLANG_ARGS correctly for bindgen.
    rustPlatform.bindgenHook
  ];

  # direnv (`use nix`) caches the env from the nix-shell invocation that computed it,
  # including the ephemeral TMPDIR nix-shell creates and deletes on exit. Later shells
  # reusing that cached env point sccache at a TMPDIR that's already gone - pin it.
  shellHook = ''
    export TMPDIR="/tmp"

    export RUSTC_WRAPPER=${pkgs.sccache}/bin/sccache
    export CC="${pkgs.sccache}/bin/sccache ${pkgs.clang}/bin/clang"
    export CXX="${pkgs.sccache}/bin/sccache ${pkgs.clang}/bin/clang++"
  '';
}
