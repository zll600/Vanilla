{
  blend = {
    prefix = ["~/.config/test-plain/"],
    files = [
      {
        name = "config.toml",
        from_config = {
          key = "value",
          number = 42,
          nested = {
            inner = true,
          },
        },
      },
    ],
  },
}
