# mkDerivation as in Nix's tests/functional/config.nix. env.nix comes from
# the Store fixture: absolute bash and coreutils, the system and a nonce.
let
  env = import ./env.nix;
in
rec {
  inherit (env) shell path system;

  mkDerivation =
    args:
    derivation (
      {
        inherit system;
        inherit (env) nonce;
        builder = shell;
        args = [
          "-e"
          (args.builder or (builtins.toFile "builder-${args.name}.sh" ''eval "$buildCommand"''))
        ];
        PATH = path;
      }
      // removeAttrs args [ "builder" ]
    );
}
