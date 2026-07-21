{pkgs, managerPackage, managerBinary, config, ...}:
let
  configFile = pkgs.writeText "declarative-manager-config.json" (builtins.toJSON config);
in
pkgs.writeShellApplication {
  name = managerBinary;
  runtimeInputs = [managerPackage];
  text = ''
    exec ${managerPackage}/bin/${managerBinary} --config ${configFile} "$@"
  '';
}
