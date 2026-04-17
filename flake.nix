{
  inputs.flakelight-rust.url = "github:accelbread/flakelight-rust";
  outputs = {flakelight-rust, ...}:
    flakelight-rust ./. {
      devShell.packages = pkgs: (with pkgs; [sqlx-cli]);

      devShell.env = {
        DATABASE_URL = "postgres://postgres:postgres@localhost:5432/postgres";
      };
    };
}
