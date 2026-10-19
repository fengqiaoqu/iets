{
  outputs =
    { self }:
    let
      inherit (import ./config.nix) system;
      pkgs = {
        chain = import ./chain.nix { };
        multi = import ./multi.nix;
        default = pkgs.chain;
      };
    in
    {
      packages.${system} = pkgs;
      checks.${system} = { inherit (pkgs) chain multi; };
      devShells.${system}.default = import ./shell.nix;
    };
}
