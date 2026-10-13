# ClipCraft

Aperta **F8** e os últimos **30 segundos** da tela viram um clipe.
Sem login, sem conta, sem upload, sem overlay. Só um `.exe` leve na bandeja.

> Feito pra meme. Quer usar, usa.

## Como usar

1. Baixe o `ClipCraft-windows.zip` em **Releases** e extraia.
2. Abra o `ClipCraft.exe` (deixe o `ffmpeg.exe` e o `ffprobe.exe` na mesma pasta).
3. Jogue. Aconteceu algo épico? **F8**. Ouviu o "plim"? Tá salvo em `Vídeos\ClipCraft`.

A janela tem só duas coisas: a explicação e o **histórico** dos clipes (assistir, mostrar na pasta, apagar).
Fechar a janela deixa ele rodando na bandeja. Pra sair: botão direito no ícone → **Sair**.

## Configurações

Dá pra trocar direto na janela:

| Opção | Valores |
|---|---|
| Duração | 15s, 30s, 60s, 90s, 120s |
| FPS | 30 ou 60 |
| Tecla | F6 a F12 |
| Som do PC | liga/desliga |

Fica salvo em `%APPDATA%\ClipCraft\config.json`.

## Por que é leve

- **Encoder da placa de vídeo**: tenta NVIDIA (NVENC) → AMD (AMF) → Intel (QuickSync) e só cai pra CPU (x264) se nenhum funcionar. O status na janela mostra qual está em uso.
- **Captura via Desktop Duplication** (`ddagrab`), direto da GPU.
- **Buffer em pedaços de 2s** que se reaproveitam em loop (`-segment_wrap`): o disco nunca enche.
- **Salvar não reprocessa o vídeo**: os pedaços são só emendados (`-c copy`), por isso é instantâneo.
- **Som do PC** via loopback do WASAPI, guardado na memória com horário de cada pedaço e colado exatamente na janela de tempo do vídeo.
- App em Rust + egui, sem Electron.

## Limitações conhecidas

- Jogos em **tela cheia exclusiva** podem sair pretos → use janela sem bordas.
- Grava só o **monitor principal**.
- Microfone ainda não entra no clipe (só o som do PC).

## Compilar

```bash
cargo build --release
```

Coloque o `ffmpeg.exe` e o `ffprobe.exe` ao lado do `target/release/ClipCraft.exe`.
O GitHub Actions (`.github/workflows/build.yml`) faz tudo sozinho: compila, baixa o FFmpeg, monta o zip e, quando você cria uma tag `v*`, publica em Releases:

```bash
git tag v0.1.0
git push origin v0.1.0
```

## Estrutura

```
src/
  main.rs      bandeja, atalho global, janela
  app.rs       interface: explicação, configurações, histórico
  recorder.rs  ffmpeg em loop + salvar clipe
  audio.rs     captura do som do PC em buffer circular
  platform.rs  coisas específicas do Windows
  config.rs    configuração em JSON
  state.rs     estado compartilhado entre as threads
```

---

Feito por **FacincaniTech**. O pacote inclui o [FFmpeg](https://ffmpeg.org) (GPL v3).
