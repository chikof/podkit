import sys
sys.path.insert(0, "/home/chiko/Projects/podkit/.worktrees/t_9f63c506/design/finalists/.build")
import contrast_core as C
def hexc(h): h=h.lstrip('#'); return tuple(int(h[i:i+2],16) for i in (0,2,4))
def tohex(c): return "#%02x%02x%02x"%tuple(max(0,min(255,round(x))) for x in c)
def find(base, bgs, need=3.1, dark=True):
    """move `base` along lightness (toward white if dark else toward black) until ratio>=need against all bgs"""
    b=hexc(base)
    for i in range(0,256):
        t=i/255
        c=tuple(x+(255-x)*t for x in b) if dark else tuple(x*(1-t) for x in b)
        if all(C.ratio(c,hexc(g))>=need for g in bgs): return tohex(c)
for name,base,bgs,dark in [
 ("01 dark","#46504a",["#0b0d0c","#101311","#161a17"],True),
 ("01 light","#7c7a68",["#f4f1e8","#ece8db","#e4dfcf"],False),
 ("02 dark","#383a40",["#0f1011","#141517","#1a1b1e"],True),
 ("02 light","#c9cad2",["#fbfbfc","#ffffff","#f4f4f6"],False),
 ("09 dark","#454c57",["#0f1115","#181b20","#1f2329"],True),
 ("09 light","#aeb4be",["#f1f2f4","#ffffff","#f6f7f8"],False)]:
    print(name, find(base,bgs,3.1,dark))
