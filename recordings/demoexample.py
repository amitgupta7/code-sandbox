print("### Native lib example ###")
import cowsay
cowsay.daemon("no error")

print("\n\n### STDOUT ###")
import sys
for inp in sys.stdin:
  print(inp)

print("\n\n### ARGUMENTS ###")
for arg in sys.argv:
  print(arg)


print("### Native lib example ###")
import humanize
print(humanize.naturalsize(12345678))
