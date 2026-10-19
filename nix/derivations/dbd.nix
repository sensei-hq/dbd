{ lib, rustPlatform }:
let
  root = ../..;
  cargoToml = builtins.fromTOML (builtins.readFile (root + "/Cargo.toml"));
in
rustPlatform.buildRustPackage {
  pname = "dbd";
  inherit (cargoToml.workspace.package) version;

  src = lib.cleanSourceWith {
    src = root;
    # Only what cargo needs: the site/, docs/ and tooling dirs would
    # otherwise invalidate the build on every unrelated edit.
    filter =
      path: type:
      let
        rel = lib.removePrefix (toString root + "/") (toString path);
      in
      builtins.any (p: rel == p || lib.hasPrefix (p + "/") rel) [
        "Cargo.toml"
        "Cargo.lock"
        "src"
        "crates"
        "tests"
        "README.md"
        "LICENSE"
      ];
  };

  cargoLock.lockFile = root + "/Cargo.lock";

  # pg_query builds libpg_query from C and generates bindings with bindgen.
  nativeBuildInputs = [ rustPlatform.bindgenHook ];

  # Only the `dbd` binary; the test suite needs a live/embedded
  # PostgreSQL and network access, neither of which the sandbox has.
  cargoBuildFlags = [ "--bin" "dbd" ];
  doCheck = false;

  meta = {
    inherit (cargoToml.workspace.package) description homepage;
    license = lib.licenses.mit;
    mainProgram = "dbd";
  };
}
