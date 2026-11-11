{
  description = "cas: versions of directories in a content-addressed store";

  inputs.nixpkgs.url = "https://channels.nixos.org/nixos-unstable/nixexprs.tar.xz";

  outputs =
    { self, nixpkgs }:
    let
      systems = [
        "x86_64-linux"
        "aarch64-linux"
      ];
      each = f: nixpkgs.lib.genAttrs systems (system: f nixpkgs.legacyPackages.${system});
    in
    {
      packages = each (pkgs: rec {
        default = cas;
        # The command line tool and the forge server, with the manual page
        # and the completions, all printed by the tool itself.
        cas = pkgs.rustPlatform.buildRustPackage {
          pname = "cas";
          version = (builtins.fromTOML (builtins.readFile ./cli/Cargo.toml)).package.version;
          src = pkgs.lib.cleanSource ./.;
          cargoLock.lockFile = ./Cargo.lock;
          cargoBuildFlags = [ "--workspace" ];
          nativeBuildInputs = [
            pkgs.installShellFiles
            pkgs.pkg-config
          ];
          buildInputs = [ pkgs.zstd ];
          # The tests start servers and run git; `nix flake check` runs them.
          doCheck = false;
          postInstall = ''
            $out/bin/cas help --man > cas.1
            installManPage cas.1
            installShellCompletion --cmd cas \
              --fish <($out/bin/cas completions fish) \
              --bash <($out/bin/cas completions bash) \
              --zsh <($out/bin/cas completions zsh)
          '';
          meta.mainProgram = "cas";
        };
      });

      checks = each (pkgs: {
        # Formatting, lints and every test: what scripts/check runs.
        tests = self.packages.${pkgs.system}.cas.overrideAttrs (old: {
          pname = "cas-tests";
          doCheck = true;
          nativeCheckInputs = [
            pkgs.git
            pkgs.clippy
            pkgs.rustfmt
          ];
          checkPhase = ''
            export HOME=$TMPDIR
            cargo fmt --all --check
            cargo clippy --release --workspace --all-targets --offline -- -D warnings
            cargo test --release --workspace --offline
          '';
        });
      });

      devShells = each (pkgs: {
        default = pkgs.mkShell {
          packages = [
            pkgs.cargo
            pkgs.rustc
            pkgs.clippy
            pkgs.rustfmt
            pkgs.git
            pkgs.nodejs
          ];
        };
      });
    };
}
