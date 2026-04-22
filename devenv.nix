{ pkgs, ... }:
{
  packages = with pkgs; [
    sqlx-cli
    resterm
    lazysql
  ];

  dotenv.enable = true;
  languages.rust = {
    enable = true;
    components = [
      "rustc"
      "cargo"
      "clippy"
      "rustfmt"
      "rust-analyzer"
    ];
  };

  services = {
    postgres = {
      enable = true;
      package = pkgs.postgresql_18;
      initialDatabases = [
        {
          name = "podkit";
          user = "podkit";
          pass = "podkit";
        }
      ];
      listen_addresses = "127.0.0.1";
      port = 5432;
    };
  };

  git-hooks.hooks = {
    rustfmt.enable = true;
    nixfmt.enable = true;
  };
}
