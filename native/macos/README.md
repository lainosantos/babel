# Babel Audio — driver HAL para macOS

Este pacote contém o driver próprio `BabelAudio.driver`, um Audio Server Plug-in
em espaço de usuário. Não precisa de BlackHole, extensão de kernel ou DriverKit.
Ele publica dois dispositivos independentes, cada um com entrada e saída:

| Dispositivo | UID estável | Uso pelo Babel |
| --- | --- | --- |
| Babel Microphone | `org.babel.audio.microphone.v1` | Escrever na saída; os aplicativos capturam sua entrada |
| Babel Speaker | `org.babel.audio.speaker.v1` | Capturar a entrada; os aplicativos reproduzem na saída |

O driver transporta áudio original ou produzido pelo aplicativo; não inclui IA,
rede, captura de hardware, gravação, transcrição ou escolha de dispositivo físico.
Sem o processo Babel fazendo o roteamento, o cabo virtual continua enumerado,
mas não encaminha sozinho para um microfone ou alto-falante real. Dispositivos
padrão do sistema não são alterados pelo pacote nem pelo desinstalador.

## Implementação e limites

- PCM nativo de 32 bits float, estéreo intercalado, fixo em 48.000 Hz. Conversão
  de taxa/formato cabe ao HAL e aos clientes; o driver recusa outra configuração
  física ou virtual de stream.
- Cada cabo mantém 16.384 quadros em memória fixa. O limite de uma chamada de IO
  é **4.096 quadros**. Solicitações maiores retornam erro ao HAL antes de acessar
  o buffer; não são processadas parcialmente.
- A latência anunciada na entrada é **4.096 quadros, aproximadamente 85,33 ms**.
  Essa margem conservadora suporta inclusive `ReadInput` antes de `WriteMix`
  num ciclo com o maior bloco aceito. Ela se soma às filas do aplicativo e ao
  tempo do modelo. Não se presume que o HAL renderize blocos adiantados.
  Reduzir essa margem exige comprovar o agendamento em hardware macOS; este
  código não promete latência de poucos milissegundos.
- Um relógio comum usa `mach_absolute_time` e a conversão racional de seu
  timebase, quantizando timestamps em 512 quadros. Não existe thread de timer.
  O timestamp não depende da velocidade com que o consumidor lê.
- O buffer é endereçado pelo tempo de amostra e tem uma geração por ciclo de
  uso. Vários leitores recebem os mesmos quadros sem consumir uma fila.
  Lacunas, sobrescrita e dados de uma geração antiga retornam silêncio. Ao
  parar o último cliente, ou alterar a atividade de um stream, a geração muda.
- `WriteMix` é o áudio misturado pelo HAL. O driver não soma repetidamente
  buffers de clientes. Uma admissão atômica sem espera rejeita um escritor
  concorrente inesperado; processamento de amostras nunca espera um mutex.
- Até 256 clientes por cabo. Registro, remoção, `StartIO` e `StopIO` usam um
  mutex curto de controle para impedir duplicação de clientes e contagens
  inconsistentes. Processamento de amostras e relógio não adquirem esse mutex.
- O núcleo `babel-hal-core` proíbe `unsafe`. A crate separada `babel-hal-driver`
  concentra os ponteiros FFI. `abi.c`, compilado com o SDK da Apple, declara o
  vtable HAL real, monta ASBD/layouts e extrai `IOCycleInfo`. Nenhum layout de
  estrutura privada do CoreAudio é reimplementado em Rust.
- Os callbacks de áudio não alocam, registram logs, fazem chamadas de rede ou
  filesystem. Seus loops têm limites fixos. Perfis de produção abortam em caso
  de panic, evitando unwind através de C; os caminhos de IO validam tamanhos,
  IDs e ponteiros nulos, e não contêm operações deliberadamente panicking.
  Um erro numa dependência/ABI continua podendo derrubar o processo que hospeda
  o plug-in. Isso não equivale a afirmar que CoreAudio ou a ponte C são memory safe.

A ABI segue o contrato público e foi conferida contra o exemplo oficial
[Creating an Audio Server Driver Plug-in](https://developer.apple.com/documentation/coreaudio/creating-an-audio-server-driver-plug-in).
O aviso de licença do exemplo consultado está em
[LICENSE-Apple-example.txt](LICENSE-Apple-example.txt). A ponte e o núcleo deste
pacote implementam loopback próprio; o exemplo NullAudio da Apple apenas produz
silêncio e descarta saída. O código original Babel usa [MIT](LICENSE-MIT.txt);
ambos os avisos acompanham o bundle gerado.

## Compilar no macOS

Requisitos: Rust com Cargo, Python 3.9+, Xcode ou Command Line Tools selecionados
por `xcode-select`, SDK macOS, `codesign`, `lipo` e `pkgbuild`. O alvo mínimo do
bundle é macOS 11.0. O aplicativo Babel pode exigir uma versão posterior para
outras funções; esse mínimo não reduz os requisitos do aplicativo.

Para desenvolvimento/CI, incluindo teste da ABI no próprio processo e pacote:

```bash
rustup target add aarch64-apple-darwin x86_64-apple-darwin
python3 native/macos/build.py --unsigned --arch universal --test --pkg
```

`--unsigned` é uma escolha explícita de desenvolvimento: o bundle recebe uma
assinatura ad-hoc, e o `.pkg` não possui certificado de distribuição. Isso permite
verificar o artefato em CI, mas não garante que o macOS de um usuário aceite
carregá-lo como driver. Não desative SIP, Gatekeeper nem outros controles do
sistema para distribuir esse build.

Também são aceitos `--arch native`, `--arch arm64` e `--arch x86_64`. Para testar
com `--test`, o bundle precisa conter a arquitetura do Mac atual. As duas slices
são compiladas separadamente com o SDK e reunidas por `lipo`; o teste executa
somente a slice correspondente ao hardware do runner.

Saídas padrão:

```text
native/macos/dist/BabelAudio.driver
native/macos/dist/BabelAudio-0.1.0.pkg
native/macos/dist/BabelAudio.pkg
native/macos/dist/uninstall.sh
```

`BabelAudio.pkg` é uma cópia estável para a distribuição junto ao aplicativo,
por exemplo `drivers/macos/BabelAudio.pkg`. Distribua também o desinstalador em
`drivers/macos/uninstall.sh`. O build usa o workspace Cargo isolado desta pasta;
não altera o manifesto, a política `forbid(unsafe_code)` ou os binários do app.
Não instala targets/toolkits automaticamente, não usa `sudo`, não instala o
pacote e não reinicia o CoreAudio.

## Assinar, notarizar e distribuir

Para distribuição, obtenha as identidades **Developer ID Application** e
**Developer ID Installer** da sua conta Apple Developer e mantenha-as no
keychain da máquina de build. Credenciais e certificados não ficam no repositório.
Depois de configurar um perfil de credenciais do `notarytool`:

```bash
python3 native/macos/build.py --arch universal --test --pkg \
  --sign-identity 'Developer ID Application: SUA ORGANIZACAO (TEAMID)' \
  --installer-identity 'Developer ID Installer: SUA ORGANIZACAO (TEAMID)' \
  --notary-profile 'babel-notary'
```

O script verifica a assinatura e o símbolo exportado da factory, assina o pacote,
submete-o explicitamente à Apple, aguarda o resultado e anexa/valida o ticket.
Sem `--notary-profile` não há upload. Sem credenciais de distribuição e execução
bem-sucedida desses passos, o artefato não está declarado pronto para entrega.
O driver HAL não solicita entitlements de AudioDriverKit, que é outro modelo de
driver. Veja as instruções oficiais de
[empacotamento](https://developer.apple.com/documentation/xcode/packaging-mac-software-for-distribution)
e [notarização](https://developer.apple.com/documentation/security/notarizing-macos-software-before-distribution).

## Instalar e remover

Abra o `.pkg` no Installer do macOS e autorize a instalação como administrador.
O destino fixo é `/Library/Audio/Plug-Ins/HAL/BabelAudio.driver`. O pacote impede
realocação, verifica a identidade de uma instalação anterior, rejeita symlinks
nos caminhos esperados e atribui os arquivos a `root:wheel`. **Reinicie o macOS**
para carregar o driver e confira os dispositivos no Audio MIDI Setup. O
instalador não interrompe automaticamente uma chamada em andamento.

Para remover, encerre o Babel e aplicações que usam os dispositivos, e execute
explicitamente:

```bash
sudo sh native/macos/uninstall.sh
```

O script exige privilégios já concedidos, verifica o caminho fixo e o bundle ID,
remove somente o driver Babel e seu recibo de instalação. Reinicie o macOS para
descarregar a cópia que o CoreAudio ainda mantém em memória. Nenhum destes
scripts reinicia `coreaudiod`, altera dispositivos padrão ou remove outros
plug-ins de áudio.

## Verificação sem instalar ou usar hardware

No Linux/macOS/Windows, os testes portáveis do workspace exercitam o núcleo,
propriedades e o contrato privado Rust/C:

```bash
cargo test --manifest-path native/macos/Cargo.toml --all-targets --locked
cargo clippy --manifest-path native/macos/Cargo.toml --all-targets --locked -- -D warnings
```

No macOS, `build.py --test` adicionalmente compila o shim contra o SDK e usa
`CFPlugInCreate`/`CFPlugInInstanceCreate` para carregar o bundle no processo de
teste, sem instalá-lo. Exercita enumeração, UIDs, formatos, clock, loopback real
em buffers de memória, leitores repetidos, isolamento dos cabos, rejeição de
buffers grandes e ausência de replay após parada. Não usa o microfone físico
nem os padrões de áudio do host.

**Situação desta entrega:** testes Rust passaram no host Linux. Compilação com
SDK, assinatura, instalação e áudio real em hardware macOS ainda precisam ser
executados; arquivos de CI não são prova de que esses passos já passaram.

Antes de liberar um instalador, valide em Macs Intel e Apple Silicon:

1. Instalar pacote assinado/notarizado, reiniciar e verificar os dois UIDs e
   streams nos dois sentidos. Confirmar que os dispositivos físicos e padrões
   anteriores foram preservados.
2. Reproduzir um sinal conhecido em Babel Speaker e capturá-lo pela entrada
   correspondente, com dois leitores simultâneos. Verificar canais, conteúdo,
   ausência de dados do outro cabo e atraso medido versus 4.096 quadros.
3. Repetir no cabo Babel Microphone com blocos de 64, 128, 256, 512, 1.024, 2.048 e
   4.096 quadros; usar um cliente de teste que varie o tamanho e a ordem de IO.
4. Repetir ligar/desligar clientes e trocar o destino para dispositivo físico no
   meio da sessão. Confirmar silêncio inicial, ausência de replay e retorno do
   app à política de somente rotear dispositivos virtuais efetivamente em uso.
5. Testar carga prolongada, suspensão/retomada, encerramento do app e remoção do
   pacote. O teste de contrato não substitui a validação do host HAL real.
