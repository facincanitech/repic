# RePic

Aperta **F8** e os últimos **30 segundos** da tela viram um clipe.
Sem login, sem conta, sem upload, sem overlay. Só um `.exe` leve na bandeja.

> Vídeo é só um monte de pic em sequência. Feito pra meme. Quer usar, usa.

## Como usar

1. Baixe o `RePic-windows.zip` em **Releases** e extraia.
2. Abra o `RePic.exe` (deixe o `ffmpeg.exe` e o `ffprobe.exe` na mesma pasta).
3. Jogue. Aconteceu algo épico? **F8**. Ouviu o "plim"? Tá salvo em `Vídeos\RePic`.

A janela mostra os **últimos clipes** em grade: clique na miniatura pra assistir,
ou use **Pasta**, **Exportar** e **Excluir** embaixo de cada um.
Minimizar manda pra barra de tarefas; fechar deixa ele rodando na bandeja, perto do relógio.
Nos dois casos ele continua gravando e o atalho continua valendo. Pra sair: botão direito no ícone → **Sair**.

O app fala português, inglês ou espanhol, conforme o idioma do Windows.

## Configurações (⚙)

| Opção | Valores |
|---|---|
| Atalho | qualquer tecla, sozinha ou com Ctrl/Alt/Shift (a tecla continua funcionando no jogo) |
| Duração | 15s, 30s, 60s, 90s, 120s |
| Qualidade | 30 ou 60 fps |
| Monitor | o principal do Windows ou qualquer outro |
| Clipar em qualquer lugar | ligado: o atalho vale sempre. Desligado: só com jogo/vídeo em tela cheia |
| Som do PC | liga/desliga |
| Microfone | liga/desliga (usa o microfone padrão do Windows) |
| Iniciar com o Windows | abre escondido na bandeja, já gravando |
| Pasta dos clipes | onde você quiser |

Fica salvo em `%APPDATA%\RePic\config.json`.

## Por que é leve

- **Encoder da placa de vídeo**: usa o da placa onde o monitor está ligado (NVIDIA NVENC, AMD AMF ou Intel QuickSync) e só cai pra CPU (x264) se nenhum funcionar. O status na janela mostra qual está em uso.
- **Captura via Desktop Duplication** (`ddagrab`), direto da GPU. Em notebook com duas placas, cada monitor é capturado pela placa dele.
- **Buffer em pedaços de 2s** que se reaproveitam em loop (`-segment_wrap`): o disco nunca enche.
- **Salvar não reprocessa o vídeo**: os pedaços são só emendados (`-c copy`), por isso é instantâneo.
- **Som do PC e microfone** via WASAPI, guardados na memória e colados exatamente na janela de tempo do vídeo.
- **Atalho por gancho de teclado**: só observa a tecla, não rouba ela do jogo.
- App em Rust + egui, sem Electron.

## Limitações conhecidas

- Jogos em **tela cheia exclusiva** podem sair pretos → use janela sem bordas.
- Jogo rodando **como administrador**: o Windows não deixa o atalho ver as teclas. Rode o RePic como administrador também, ou use o botão / o menu da bandeja.

## Compilar

```bash
cargo build --release
cargo test
```

Coloque o `ffmpeg.exe` e o `ffprobe.exe` ao lado do `target/release/RePic.exe`.
O GitHub Actions (`.github/workflows/build.yml`) faz tudo sozinho: testa, compila, baixa o FFmpeg, monta o zip e, quando você cria uma tag `v*`, publica em Releases:

```bash
git tag v0.2.0
git push origin v0.2.0
```

## Estrutura

```
src/
  main.rs      bandeja, janela, liga tudo
  app.rs       interface: grade de clipes, configurações, excluir
  recorder.rs  ffmpeg em loop + salvar clipe
  audio.rs     som do PC e microfone em buffer circular
  hotkey.rs    atalho global (gancho de teclado)
  lang.rs      textos em português, inglês e espanhol
  platform.rs  coisas específicas do Windows (monitores, registro, diálogos)
  config.rs    configuração em JSON
  state.rs     estado compartilhado entre as threads
```

---

Feito por **FacincaniTech**. O pacote inclui o [FFmpeg](https://ffmpeg.org) (GPL v3).
