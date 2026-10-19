# Every top-level derivation of nixpkgs, for tests/nixpkgs_eval.rs.
{ nixpkgs, limit ? null }:
let
  pkgs = import nixpkgs { };
  inherit (pkgs) lib;
  isDrv =
    n:
    let
      r = builtins.tryEval (lib.isDerivation pkgs.${n});
    in
    r.success && r.value;
  names = builtins.filter isDrv (builtins.attrNames pkgs);
  # a NixOS system, whose evaluation reaches most of the module system
  nixos = import (nixpkgs + "/nixos/lib/eval-config.nix") {
    system = "x86_64-linux";
    modules = [
      (
        { modulesPath, ... }:
        {
          imports = [ (modulesPath + "/profiles/minimal.nix") ];
          boot.loader.grub.enable = false;
          fileSystems."/" = {
            device = "none";
            fsType = "tmpfs";
          };
          networking.hostName = "scratch";
          system.stateVersion = "25.11";
        }
      )
    ];
  };
in
lib.genAttrs (if limit == null then names else lib.take limit names) (n: pkgs.${n})
// {
  nixos-toplevel = nixos.config.system.build.toplevel;
}
