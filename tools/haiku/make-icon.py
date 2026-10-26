#!/usr/bin/env python3
"""Original Haiku page and fountain pen. Rebuild with python3 tools/haiku/make-icon.py."""
from icon_art import Drawing, INK, plane

d = Drawing()
p = plane(9, 4)
d.shadow([(9,46),(42,58),(61,58),(32,46)])
# Paper thickness and folded corner; darkest surface faces right.
d.shape([(42,16),(46,12),(46,53),(42,58)], d.gradient('abb9c5','647586'), INK)
d.shape(p([(0,0),(25,0),(33,8),(33,42),(0,42)]), d.gradient('fffffa','d4dce1'), INK)
d.shape(p([(25,0),(25,8),(33,8)]), d.gradient('d6e6ef','91a6b5'), '647586', 1)
d.shape(p([(5,10),(22,10),(22,15),(5,15)]), d.gradient('8eb9ed','3e6aaf'))
for v, end in [(21,27),(27,27),(33,20)]:
    d.line(p([(5,v),(end,v)]), '6d7e8e', 1.8)
# Blue pen, with a silver cap, gold nib and a dark right-hand facet.
d.shape([(27,53),(43,20),(50,23),(35,55),(27,58)], d.gradient('9cc7f4','3565a6'), INK)
d.shape([(46,22),(50,23),(35,55),(32,54)], '294c82')
d.shape([(43,20),(46,14),(53,17),(50,23)], d.gradient('eff5f7','9caab8'), INK)
d.shape([(27,53),(35,55),(27,60),(25,60)], d.gradient('ffe7a0','bc8836'), INK, 1.4)
d.line([(27,58),(30,55)], INK, 1.2)
d.save(__file__, 'wordcraft', 'WordCraft')
