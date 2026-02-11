# Zsh shell configuration

let { Order, .. } = import "../order.contract.ncl" in
{
  blend = {
    prefix = ["~/"],
    files = [
      {
        from_file = ".zshrc",
      },
      {
        from_file = ".zshenv",
      },
    ],
    when = { os = ["darwin", "linux"] },
  },
} | Order
