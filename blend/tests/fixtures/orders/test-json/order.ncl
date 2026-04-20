{
  blend = {
    prefix = ["~/.config/test-json/"],
    files = [
      {
        name = "settings.json",
        from_config = {
          fontSize = 14,
          tabSize = 2,
          fontFamily = "JetBrains Mono",
        },
      },
    ],
  },
}
