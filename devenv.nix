{ pkgs, ... }:

{
  dagger.enable = true;
  env.DAGGER_X_RELEASE = "v1.0.0-beta.14";

  packages = with pkgs; [
    lld
    cargo-audit
    cargo-deny
    cargo-hack
    cargo-nextest
    cargo-release
    cargo-watch

    rumdl
  ];

  languages = {
    rust = {
      enable = true;
      channel = "stable";
      targets = [ "wasm32-unknown-unknown" ];
    };
  };
}
