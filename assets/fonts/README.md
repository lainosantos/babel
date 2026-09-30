# Native feedback typography

`manrope-medium.ttf` is the weight-500 static instance of the existing
`ui/fonts/manrope.ttf` variable Manrope font. It uses the same SIL Open Font
License in `ui/fonts/OFL.txt`; no font is fetched at runtime.

The native CPU text rasterizer does not select variable font axes, so bundling
one 98 KB static weight makes small status text legible and removes unused font
variation data. Recreate it using FontTools (a development tool only):

```python
from fontTools.ttLib import TTFont
from fontTools.varLib.instancer import instantiateVariableFont

font = TTFont("ui/fonts/manrope.ttf")
font = instantiateVariableFont(font, {"wght": 500}, inplace=True)
font.save("assets/fonts/manrope-medium.ttf")
```
