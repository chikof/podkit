{ pkgs, ... }: {
  packages = with pkgs; [
    sqlx-cli
    resterm
    lazysql
    sccache
    bun
    nixpacks
  ];

  env.RUSTC_WRAPPER = "${pkgs.sccache}/bin/sccache";

  processes = {
    sccache.exec = "sccache --start-server";
  };

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

          initialSQL = ''
            ALTER SCHEMA public OWNER TO podkit;
            GRANT ALL ON SCHEMA public TO podkit;
          '';
        }
      ];
      listen_addresses = "127.0.0.1";
      port = 5432;
    };
  };

  git-hooks.hooks = {
    rustfmt.enable = true;
    nixfmt.enable = true;

    clippy = {
      enable = true;
      entry = "env SQLX_OFFLINE=true cargo clippy --workspace --all-targets --all-features -- -D warnings";
      pass_filenames = false;
    };

    dashboard-lint = {
      enable = true;
      name = "dashboard-lint";
      entry = "bash -c 'cd dashboard && bun run lint'";
      files = "^dashboard/.*\\.(ts|svelte|js|css|json)$";
      pass_filenames = false;
      language = "system";
    };
  };
}
