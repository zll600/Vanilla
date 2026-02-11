# Bash shell configuration

let { Order, .. } = import "../order.contract.ncl" in
{
  blend = {
    prefix = ["~/"],
    files = [
      {
        from_file = ".bashrc",
      },
      {
        from_file = ".profile",
      },
    ],
    when = { os = ["darwin", "linux"] },
  },
} | Order
