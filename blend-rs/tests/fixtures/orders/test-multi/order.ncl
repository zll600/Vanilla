{
  blend = {
    prefix = ["~/.config/test-multi/"],
    files = [
      {
        name = "config.toml",
        from_config = {
          theme = "dark",
          font_size = 14,
        },
      },
      {
        name = "extra.txt",
        from_file = "extra.txt",
      },
    ],
  },
}
