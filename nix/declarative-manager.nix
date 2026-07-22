{
  pkgs,
  managerPackage,
  managerBinary,
  config,
  extraRuntimePackages ? [],
  ...
}:
let
  configFile = pkgs.writeText "declarative-manager-config.json" (builtins.toJSON config);
in
pkgs.writeShellApplication {
  name = managerBinary;
  runtimeInputs = [managerPackage] ++ extraRuntimePackages;
  text = ''
    exec ${managerPackage}/bin/${managerBinary} --config ${configFile} "$@"
  '';
}
