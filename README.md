# SORT-rs

Port em Rust puro do [SORT](https://github.com/JiayinCao/SORT) (Simple Open-source Ray Tracing,
de Jiayin Cao), com foco em render arquitetônico. Sem plugin de Blender: as cenas entram por
glTF 2.0.

Trabalho derivado do SORT, distribuído sob a mesma licença (GPLv3, ver `LICENSE`).

## Uso

```
cargo build --release

sort --demo -o demo.png                         # interior de teste embutido
sort casa.glb -o casa.png -w 1920 --spp 512 --depth 8
sort casa.glb --env ceu.hdr --env-rotate 90     # iluminação por HDRI
sort casa.glb --sun 135,40 --sky 1.2,1.6,2.2    # sol (azimute, elevação) + céu constante
sort casa.glb -o casa.exr                       # saída linear em EXR
sort --help
```

Saídas `.png`/`.jpg` recebem tone mapping (ACES por padrão) e sRGB; `.exr`/`.hdr` ficam lineares.

## O que foi portado

| SORT (C++)                                   | SORT-rs                | Observações |
|----------------------------------------------|------------------------|-------------|
| `integrator/pathtracing`, `integratormethod` | `integrator.rs`        | Path tracing com amostragem de luz + MIS, roleta russa; ver "Ruído em interiores" |
| `accel/bvh`, `bvh_utils`                     | `bvh.rs`               | SAH com 16 bins; árvore em array, travessia iterativa |
| `shape/triangle`                             | `scene.rs`             | Interseção watertight |
| `camera/perspective`                         | `camera.rs`            | Com profundidade de campo |
| `bsdf/disney`, `microfacet`, `fresnel`, `lambert`, `transparent` | `bsdf.rs` | Disney completo menos SSS; GGX anisotrópico |
| `scatteringevent`                            | `bsdf.rs`              | Mistura de BXDFs com pesos de avaliação e amostragem |
| `light/distant`, `pointlight`, `spot`, `skylight`, `hdrskylight`, `area` | `light.rs` | Luz de área = triângulos emissivos |
| `material` + TSL                             | `material.rs`          | Sem linguagem de shading: parâmetros fixos (PBR do glTF) viram closures |
| formato binário do exportador Blender        | `gltf_loader.rs`       | Malhas, materiais, texturas, câmeras, `KHR_lights_punctual` |

## Ruído em interiores

O integrador saiu do port literal do SORT nos pontos em que um interior gera ruído:

- **Um raio de sombra por tipo de luz** (cada sol, céu, luzes pontuais, superfícies emissivas),
  em vez de uma luz sorteada pela potência. Entre luminárias, e entre luzes pontuais, sorteia-se
  a que mais contribui no ponto (resampled importance sampling).
- **Portais:** o céu é amostrado mirando nas aberturas. Vidro fino já é portal; aberturas sem
  vidro se marcam com `SceneBuilder::add_portal` ou, no glTF, com um material cujo nome começa
  com `portal` (a geometria dele não é renderizada).
- **Pontos de sol:** cada caminho recebe um ponto iluminado pelo sol através de um portal e
  conecta seus vértices a ele, o que resolve a luz do sol rebatida no piso.
- **MIS no raio de continuação**, sem o raio extra de BSDF por vértice do SORT.
- **`--filter-glossy`** (padrão 0,5): superfícies vistas depois de um rebote difuso ficam pelo
  menos tão borradas quanto esse rebote, o que troca fireflies de cáusticas por luz suave. É a
  única mudança não exata: na cena de teste escurece a imagem em 2,6%. `--filter-glossy 0`
  desliga.

Na cena `--demo` (320x180, profundidade 8), em tempo igual (3,3 s), o erro relativo médio contra
uma referência caiu de 0,041 para 0,0057. As ferramentas da medição estão em `examples/`:
`compare` (erro contra uma referência) e `variance` (variância por pixel, uma luz de cada vez).

## Diferenças deliberadas em relação ao SORT

- **Vidro fino** (transmissão sem `KHR_materials_volume`): transmissão reta + reflexão dielétrica,
  e raios de sombra atravessam atenuados. É o que deixa o sol entrar por uma janela.
- Epsilon dos raios de sombra de 1 cm para 0,1 mm (1 cm vaza luz em todos os cantos de um
  ambiente modelado em metros).
- Profundidade máxima da BVH de 16 para 48 (cenas com milhões de triângulos).
- Correções de consistência em `MicrofacetRefraction` (termos chi+), no pdf difuso do Disney e
  na mistura de BXDFs do `ScatteringEvent`; estão comentadas no código.

## Ainda não portado

Meios participantes, SSS, cabelo/pelo, tecido, MERL/Fourier, coat/double-sided, os outros
integradores (bidirecional, light tracing, instant radiosity, AO, Whitted), os outros
aceleradores (QBVH/OBVH, KD-tree, octree, grid), SIMD, câmeras ortográfica e de ambiente.
Do lado do glTF: recorte por alpha, `KHR_texture_transform` e clearcoat.

## Testes

`cargo test` roda 18 testes: consistência entre amostragem, valor e pdf de cada BXDF, BVH contra
força bruta, câmera, mapa de ambiente, loader glTF e renders comparados com resultados analíticos.
