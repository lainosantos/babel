# Babel

Microfone e saída de áudio virtuais para tradução bidirecional de voz. O núcleo,
os adaptadores de IA, o servidor do painel e os controles de bandeja são Rust.
Cada direção pode usar um provedor e uma voz diferentes para tradução.
A transcrição também escolhe seu próprio provedor e idioma para o microfone e
para o áudio recebido, independentemente da tradução.
Tradução, transcrição e gravação são independentes. Fora de uma sessão, o Babel
encaminha o áudio original entre os dispositivos configurados. Desligar a
tradução de uma direção mantém esse encaminhamento durante a sessão também.

O agente de comandos usa **somente o microfone físico original**, com nome de
ativação configurável (padrão “Babel”), Whisper local, **Needle 3** e integrações
MCP HTTP/stdio autenticadas. Funciona também sem sessão de tradução ou gravação.
O áudio da saída nunca alimenta o agente. Consulte
[instalação e comandos de voz](docs/voice-commands.md) e
[integrações e autenticação MCP](docs/mcp.md); os dois serviços locais precisam
ser preparados antes do uso. O painel e as notificações mostram o processamento
e o resultado sem bloquear o roteamento de áudio.

```text
Microfone físico → tradução → Babel Microphone → aplicativo de chamada
Aplicativo de chamada → Babel Speaker → tradução → fones físicos
```

O modo nativo transmite áudio em conexões persistentes. Gemini Live Translate e
OpenAI Realtime Translate são os adaptadores de tradução contínua. Modelos
conversacionais esperam detecção de fim de fala; a opção local trabalha em trechos.
Vozes personalizadas acrescentam síntese TTS em streaming após a tradução.
**Streaming não significa latência zero nem garante um prazo de tempo real rígido.**

## Começar

Requer Rust **1.90 ou posterior**. Clone/abra este diretório e compile:

```sh
cargo build --release --locked
cargo run --release -- init
```

No Linux, instale `pulseaudio-utils` (Ubuntu/Debian), com PipeWire + pipewire-pulse
ou PulseAudio em execução na sessão do usuário. Não execute o Babel como root.

```sh
sudo apt install pulseaudio-utils
cargo run --release -- setup
cargo run --release -- serve
```

O sistema escolhe uma porta livre para o painel em cada execução. O terminal
imprime o endereço efetivo com um token de sessão; abra esse link completo ou
**Configurações** na bandeja, que acompanha o endereço atual. A bandeja oferece
**configurações**, **iniciar sessão**, **encerrar
sessão** e **sair**. Encerrar uma sessão finaliza seus arquivos e volta ao áudio
original; sair do Babel encerra também o encaminhamento local.
Também permite escolher o **microfone físico** e a **saída física**, com a
tradução parada ou em execução, e atualizar a lista de dispositivos. A troca
mantém a sessão de IA e os arquivos abertos; pode haver uma breve lacuna de áudio.
O painel também funciona sem bandeja com `serve --no-tray`.

O painel e a bandeja identificam o sistema em que o Babel está executando.
As instruções de áudio, os controles de dispositivos virtuais e a orientação
de início automático acompanham esse sistema. No Linux, o painel oferece criar
e remover os dispositivos Babel; no macOS e Windows, mostra como preparar e
instalar o pacote do driver Babel próprio e selecionar suas pontas. A ajuda abre
diretamente na seção do sistema atual.

Em Linux/macOS, `./scripts/run.sh` inicia o binário compilado a partir deste
diretório; na primeira execução, compila caso ele ainda não exista. O script
também aceita os subcomandos, por exemplo `./scripts/run.sh doctor`.

1. Em **Tradução e vozes**, configure os perfis usados para traduzir.
   Provedores de nuvem recebem uma chave temporária pelo painel ou pela variável
   de ambiente indicada. Só gravar ou encaminhar originais dispensa essa etapa.
2. Nessa mesma página, escolha o tradutor de cada faixa. As configurações de um
   provedor não sobrescrevem as dos outros. O padrão é Gemini Live Translate.
3. Em **Roteamento**, selecione os físicos e as pontas virtuais do seu sistema.
   No Linux, use o microfone físico como captura e `babel_mic_bus` como reprodução
   do microfone; na saída, `babel_speaker.monitor` como captura e seus fones como
   reprodução. O [guia por sistema](docs/platforms.md) mostra as pontas macOS/Windows.
4. Em **Tradução e vozes**, defina os idiomas e as vozes de cada direção e ative
   apenas as traduções desejadas. Os perfis de IA e a biblioteca de vozes ficam juntos.
5. Em **Transcrição**, escolha os originais que deseja guardar e, para cada
   origem, seu provider STT e idioma: Gemini, OpenAI, Deepgram ou Whisper.
   Os perfis e credenciais de transcrição ficam nessa página e são independentes
   dos perfis de tradução. Em **Gravação**, selecione o áudio original que deseja
   salvar. Ambas têm um atalho para a pasta base e os nomes de arquivos em
   **Ajustes**. É possível só gravar, só transcrever ou combinar os recursos.
6. Salve os ajustes, preencha **Nome da sessão** se desejar e clique em **Iniciar
   sessão**. No aplicativo da chamada, selecione **Babel_Microphone** e
   **Babel_Speaker** no Linux, ou **Babel Microphone** e **Babel Speaker** no
   macOS/Windows. Sem sessão ativa, esses caminhos passam o original.

Para usar somente o áudio original, deixe a tradução desligada nas duas direções.
O roteamento continua funcionando quando um aplicativo usa os dispositivos Babel.
Se quiser guardar esse áudio, ative **Gravação** e inicie uma sessão; não é preciso
escolher um provedor especial, fornecer chave de IA ou habilitar transcrição.

Configurações antigas com `loopback` são migradas para o perfil Gemini com
tradução e transcrição da rota desligadas, sem iniciar chamadas à nuvem. Veja
[a migração de configuração](docs/configuration.md#arquivos-e-chaves).

No macOS e Windows, o **driver nativo Babel** fornece dois percursos independentes.
O código-fonte e os scripts de compilação estão em `native/macos` e `native/windows`;
**a assinatura dos pacotes de distribuição e a validação em hardware nativo ainda
estão pendentes**. O app usa CoreAudio/WASAPI via CPAL, com IDs persistentes e
monitoramento dos aplicativos. No macOS, essa detecção exige **macOS 14.2+**.
BlackHole e VB-CABLE continuam como alternativas opcionais, sem serem dependências
do driver próprio. Consulte [compilação e pacotes nativos](docs/native-drivers.md)
e [o mapa de dispositivos por sistema](docs/platforms.md).

No Windows, o executável é `target\release\babel.exe`; os mesmos subcomandos
funcionam no PowerShell. No macOS, o loop de eventos da bandeja roda na thread
principal. Permita a captura de microfone ao aplicativo/terminal.

## Documentação

- [Guia de configuração e operação](docs/configuration.md): perfis, chaves,
  idiomas, rotas, qualidade, filas, transcrição, gravação e bandeja.
- [Idiomas da interface](docs/localization.md): inglês, português, seleção pelo
  sistema e inclusão de novos catálogos de tradução.
- [Transcrição dos originais](docs/transcription.md): Gemini, OpenAI, Deepgram e
  Whisper, seleção de idioma/provider por origem, perfis próprios e limitações.
- [Gravação dos originais](docs/recording.md): WAV único, mistura, sincronização
  e limites do gravador.
- [Dispositivos Linux/macOS/Windows](docs/platforms.md): instalação,
  roteamento, permissões, drivers e solução de problemas.
- [Instaladores no GitHub Actions](docs/ci-installers.md): pacotes por sistema,
  artefatos, verificações e assinatura.
- [Drivers nativos Babel](docs/native-drivers.md): código, compilação,
  preparação dos pacotes e requisitos de instalação explícita.
- [Gemini Live](docs/providers.md): protocolos, modelos, transcrições,
  capacidades e restrições da tradução contínua.
- [OpenAI, Deepgram e serviços open source](docs/other-providers.md): Realtime
  Translate, Realtime conversacional, Deepgram STT e inferência local.
- [Modelos locais integrados](docs/local-inference.md): Whisper, Qwen e Piper,
  preparação automática, armazenamento, downloads e uso offline.
- [Vozes Gemini e ElevenLabs](docs/voices.md): biblioteca, voice design,
  clonagem, seleção por faixa, formatos, requisitos, custos e limites.
- [Arquitetura, segurança de memória e desempenho](docs/architecture.md).
- [Diagnóstico e validação](docs/testing.md).
- [Iniciar a bandeja no login](docs/autostart.md): ativação opcional por usuário.
- [Configuração completa de exemplo](examples/babel.example.toml).

## O que está implementado

| Integração de tradução/voz | Tradução de áudio | Voz personalizada |
|---|---|---|
| Gemini Live Translate | Contínua, áudio para áudio | Preservação automática aproximada; TTS opcional para voz fixa |
| Gemini 3.8 Live | Speech-to-speech por turnos/VAD | Voz pronta nativa; TTS opcional |
| OpenAI Realtime Translate | Contínua, áudio para áudio | Voz do modelo; TTS opcional |
| OpenAI Realtime | Speech-to-speech por turnos/VAD | Voz pronta nativa; TTS opcional |
| Whisper + Qwen + Piper integrados | Pipeline local por trechos | Vozes Piper do catálogo; TTS de nuvem opcional |
| Gemini 3.8 TTS | Síntese do texto traduzido, não tradutor isolado | Vozes prontas, design e clonagem cadastrada |
| ElevenLabs | Síntese do texto traduzido, não tradutor isolado | Vozes da biblioteca, design e instant voice clone |

Os quatro reconhecedores abaixo produzem o TXT dos originais, com qualquer
tradutor ou sem tradução. Cada origem escolhe seu próprio provider e idioma.

| Reconhecedor STT | Modelo/caminho padrão | Falantes e tempos |
|---|---|---|
| Gemini | `gemini-3.5-transcribe-live` | Sem diarização ou tempos por palavra no streaming |
| OpenAI | `gpt-live-transcribe` | Sem identificação de falantes ou tempos por palavra no adaptador padrão |
| Deepgram | Nova-3 via Listen v1 | Diarização configurável e tempos fornecidos nas palavras |
| Whisper | Motor integrado, modelo Base; Tiny/Small opcionais | Offsets dos trechos capturados, sem identificação de falantes |

Perfis de reconhecimento usam `transcription.providers.*`; a escolha e o idioma
ficam em `transcription.microphone_recognition` e
`transcription.speaker_recognition`. O texto de entrada do tradutor STS não é
gravado nem substitui esse reconhecimento. Veja [configuração e exemplos](docs/transcription.md).

Os providers locais em modo integrado são preparados ao salvar a seleção. Os
instaladores incluem os motores; pesos ausentes são baixados com verificação de
hash e reutilizados offline. Não é preciso instalar ou iniciar Python, Ollama
ou serviços separados. Endpoints externos, incluindo Ollama, continuam opcionais
para instalações próprias. A UI acompanha preparação e permite escolher a pasta
absoluta dos modelos e as threads de reconhecimento/tradução.


A biblioteca permite criar várias vozes, listar os perfis da conta e selecionar
uma voz para cada direção. O áudio de referência/consentimento é enviado somente
quando a ação de clonagem é acionada. Chaves digitadas no painel ficam na memória
até o processo encerrar; o TOML guarda apenas o nome da variável correspondente.

## Arquivos da sessão, participantes e voz

As transcrições gravadas são **somente dos áudios originais**: um único TXT por
sessão reúne os trechos do microfone e da saída recebida, identificados por
`[microfone]` e `[saída recebida]`. A ordem é a de chegada dos fragmentos, que
pode diferir da ordem exata da fala nas duas conexões. Horários são opcionais;
o texto distingue offsets de áudio, alinhamento aproximado e recebimento local.

A gravação de áudio é outra opção, desligada por padrão. Ela mistura as origens
selecionadas em **um único WAV PCM16 mono a 16 kHz**, antes da tradução e do ganho
da saída. Transcrição e gravação têm seleções de faixas e pastas independentes.
Em **Ajustes → Arquivos da sessão**, `files.base_path` define uma pasta base
comum **sempre absoluta**; o painel exibe os destinos completos do TXT e do WAV.
Configurações novas usam a pasta `Babel` dentro da pasta pessoal do usuário,
independentemente do diretório de execução. Os destinos de transcrição e gravação
podem ser relativos a essa base ou absolutos. Configurações antigas sem base ou
com base relativa são migradas uma vez contra a pasta do TOML e salvas com caminho
absoluto; a migração não move arquivos existentes. Confira o destino se antes
iniciava o programa em uma pasta diferente da configuração.
O padrão de nome comum é `{date}-{time}-{session}-{id}`: data/hora em UTC, título
adaptado para arquivo e identificador obrigatório. As extensões `.txt` e `.wav`
são automáticas. Um recurso habilitado precisa ter ao menos uma origem selecionada
com dispositivos configurados, mesmo que sua tradução esteja desligada.
Gravar os originais não exige um provedor de IA; transcrever exige reconhecimento
de fala e pode usar nuvem conforme o perfil escolhido.
Veja [nomes e opções de gravação](docs/configuration.md#arquivos-da-sessão).

Deepgram pode atribuir IDs de falante às palavras e o Babel preserva esses
metadados na transcrição. São rótulos locais da conexão, sujeitos a erros e a
reinício após reconexão; não são nomes nem identidade persistente entre origens
ou sessões. **O cadastro automático de clones nos primeiros segundos e sua
associação aos participantes não estão implementados.** Os dois canais de áudio
não são tratados como identificação das pessoas de uma reunião. Gemini pode
aproximar as características originais da voz, sem garantir uma identidade
vocal distinta por participante. Vozes fixas/desenhadas/clonadas são selecionadas
explicitamente.

Gemini exige uma amostra de referência de 10–30 segundos e uma gravação de
consentimento da mesma pessoa para cadastrar um clone. Isso é diferente da
preservação de voz do modelo Live. Os detalhes e alternativas estão no guia de
vozes; o painel não oferece controles que fingem habilitar funções incompatíveis.

## Comandos

```text
babel                         Painel local + bandeja
babel serve --port 0           Painel em porta escolhida pelo sistema (padrão)
babel serve --no-tray          Apenas painel local
babel init                    Cria babel.toml sem sobrescrever
babel devices                 Lista IDs reais de captura/reprodução
babel setup                   Cria dispositivos Linux; orienta drivers nos demais
babel uninstall               Remove dispositivos Linux; orienta remoção nativa
babel doctor                  Diagnóstico local, sem enviar áudio à nuvem
babel run                     Executa configuração sem painel; Ctrl+C encerra
babel run --session "Reunião"  Inicia uma sessão nomeada sem painel
babel --config outro.toml …   Usa outra configuração
```

Encerrar a sessão não remove os dispositivos virtuais. Assim o aplicativo da
chamada mantém sua seleção. Os módulos Linux devem ser recriados depois que o
servidor de áudio reiniciar. O Babel não troca a saída padrão global.

No painel, **Inicialização → Iniciar Babel ao entrar no sistema** registra
somente a bandeja e o serviço local. A opção vem desligada. Ao abrir, o Babel
aguarda aplicativos usando os virtuais e encaminha o áudio original aos físicos
configurados somente enquanto houver esse uso, sem iniciar
tradução, transcrição nem gravação. É possível desfazer a opção no mesmo painel.

## Estado da validação

Há testes de protocolo com WebSocket/HTTP simulados, filas e cancelamento,
configuração, transcrição, resampling, autenticação do painel e execução em
PipeWire real com áudio sintético. Compilação cruzada verifica o código Windows.
Os drivers próprios têm testes de núcleo e caminhos de compilação separados do
aplicativo. Os testes dos drivers carregados em hardware macOS/Windows, os pacotes
assinados e as chamadas com contas/chaves reais dos provedores ainda são necessários
antes de distribuir o produto como validado para produção. Veja [a reprodução dos testes](docs/testing.md).
