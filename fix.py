import sys
lines = open('src/lib.rs').readlines()
out = []
for line in lines:
    if 'interpose!' in line:
        continue
    out.append(line)
open('src/lib.rs', 'w').writelines(out)
