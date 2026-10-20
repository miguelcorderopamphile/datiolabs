# Iconos para NSIS

Generar en Windows con ImageMagick:

```powershell
# icon.ico (256x256, 128x128, 64x64, 48x48, 32x32, 16x16)
magick icon.png -define icon:auto-resize=256,128,64,48,32,16 icon.ico

# header.bmp (150x57)
magick icon.png -resize 150x57! header.bmp

# welcome.bmp (498x312)
magick icon.png -resize 498x312! welcome.bmp
```

O usar herramientas online:
- https://icoconvert.com/
- https://convertio.co/png-ico/