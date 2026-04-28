import Config

config :sykli,
  github_receiver_port:
    System.get_env("SYKLI_GITHUB_RECEIVER_PORT", "8617") |> String.to_integer(),
  github_receiver_enabled: true
