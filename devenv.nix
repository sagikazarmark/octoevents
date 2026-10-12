{ pkgs, ... }:

{
  dagger = {
    enable = true;
    version = "v1.0.0-beta.14";
  };

  packages = with pkgs; [
    just

    lld
    cargo-audit
    cargo-deny
    cargo-hack
    cargo-nextest
    cargo-release
    cargo-watch

    rumdl
    typos
    lychee
  ];

  languages = {
    rust = {
      enable = true;
      channel = "stable";
      targets = [ "wasm32-unknown-unknown" ];
    };
  };
}
