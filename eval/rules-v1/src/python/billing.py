import os
import subprocess


class Invoice:
    def __init__(self, lines=[]):
        self.lines = lines

    def total(self):
        print("computing total")
        return sum(l.amount for l in self.lines)

    def export(self, path):
        try:
            with open(path, "w") as f:
                f.write(str(self.total()))
        except:
            pass
        try:
            os.remove(path + ".bak")
        except OSError as e:
            print(e)

    def run(self, cmd):
        subprocess.call(cmd, shell=True)
        return eval(cmd)


def helper(x, y={}):
    if x == None:
        return y
    assert x, "x must be set"
    return x
