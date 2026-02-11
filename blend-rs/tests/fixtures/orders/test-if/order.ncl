let metadata = import "blend://metadata" in
{
  blend = {
    prefix = ["~/.config/test-if/"],
    files = [
      {
        name = "config.toml",
        from_config = {
          use_gpu = if metadata.os == "darwin" then true else false,
          label = "test",
        },
      },
    ],
  },
}
