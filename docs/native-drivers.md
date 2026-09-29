# Drivers Babel para macOS e Windows

Os drivers próprios ficam em `native/macos` e `native/windows`. Eles criam os
dispositivos selecionáveis; o aplicativo Rust continua responsável por captura
física, tradução, transcrição, gravação, MCP, painel e bandeja. Não há IA, rede,
chaves de API ou gravação de arquivos no caminho de áudio dos drivers.

**Estado:** código-fonte, build e instalação estão separados por plataforma.
Isso não equivale a um pacote assinado, carregado e validado em hardware. O host
de desenvolvimento Linux consegue verificar o código portável e a compilação
cruzada Rust para Windows; SDK/WDK, assinatura e execução nativa têm validação
própria. Não distribua o driver como certificado antes desses passos.

## Limite entre Rust e código específico do sistema

| Parte | Implementação | Por quê |
|---|---|---|
| Aplicativo, IA, roteamento e configurações | Rust com `forbid(unsafe_code)` | Não precisa entrar no driver nem ser reescrito. |
| Buffers dos dois drivers | Rust; armazenamento fixo e testes portáveis | Evita alocação por callback e controla limites, silêncio e descarte. |
| macOS AudioServerPlugIn | Rust, com ponte C compilada contra o SDK Apple | A ponte define a vtable/estruturas e objetos CoreFoundation conforme o SDK, sem duplicar layouts complexos à mão. |
| Windows WaveRT/PortCls | Adaptação C++ do SimpleAudioSample Microsoft + transporte Rust `no_std` | O WDK fornece essa integração em interfaces C++/COM, DMA e IRQL. O código C++ fica nessa fronteira. |
| Instalador Windows | Rust + módulo SetupAPI isolado | Cria a instância ROOT, verifica o pacote e remove apenas a instalação Babel. Não depende do DevCon. |
| Empacotamento | Python, PowerShell e shell de build | Orquestram Rust e as ferramentas oficiais do SDK; não fazem processamento de áudio. |

Chamadas FFI que usam ponteiros são explicitamente isoladas e documentadas. O
núcleo seguro não torna o HAL, o kernel ou o código C/C++ integralmente memory
safe. O driver Windows, em particular, deve passar por Driver Verifier antes
de distribuição. Um defeito nessa fronteira pode comprometer a sessão de áudio
ou o sistema; uma checagem Rust no Linux não comprova o comportamento do WDK.

## Dispositivos e roteamento

| Uso | macOS | Windows |
|---|---|---|
| Babel reproduz o microfone processado | saída **Babel Microphone** | **Babel Microphone Feed** |
| Aplicativo da chamada captura | entrada **Babel Microphone** | **Babel Microphone** |
| Aplicativo da chamada reproduz | saída **Babel Speaker** | **Babel Speaker** |
| Babel captura a saída original da chamada | entrada **Babel Speaker** | **Babel Speaker Monitor** |

No macOS cada dispositivo é duplex, com entrada e saída. Os UIDs são
`org.babel.audio.microphone.v1` e `org.babel.audio.speaker.v1`. No Windows o
adaptador é `ROOT\BabelAudio`, interface **Babel Audio v1**, com quatro pontas.
A enumeração persiste os IDs nativos; a identificação Windows usa descrição
do driver e da interface, não o apelido amigável editável. Pares ausentes ou
ambíguos mantêm o roteamento fechado.

O formato do transporte é estéreo a 48 kHz: `f32` no HAL e PCM16 no WaveRT.
O sistema de áudio pode converter formatos dos aplicativos. A camada de IA
do Babel continua trabalhando no formato de cada provider, fora do driver.
O HAL usa uma margem conservadora de 4.096 quadros (85,33 ms) para tolerar
leitura antes da escrita em um ciclo. Essa latência fixa se soma à tradução;
reduzi-la exige validar o agendamento no macOS. O WaveRT mantém até 4.096
quadros na fila, sem introduzir essa mesma margem fixa deliberadamente.
O driver transporta cada cabo localmente; não conecta os cabos entre si nem
abre dispositivos físicos. Quando o processo Babel está fechado, os virtuais
continuam instalados, mas não existe um serviço de IA/roteamento por trás deles.

## Build, pacote e instalação

O [CI do GitHub](ci-installers.md) compila os drivers em runners hospedados e
gera instaladores completos do aplicativo por sistema, com os arquivos do
driver correspondente e verificação do conteúdo. Os pacotes de CI macOS/Windows
são identificados como desenvolvimento, sem assinatura de distribuição.

Consulte os comandos completos e artefatos em:

- `native/macos/README.md`: workspace Rust, ponte compilada pelo SDK, bundle
  `BabelAudio.driver`, pacote `BabelAudio.pkg`, assinatura e remoção.
- `native/windows/README.md`: revisão Microsoft fixada e verificada por hashes,
  Visual Studio/WDK, transporte Rust, INF/CAT/SYS e instalador Rust x64/ARM64.

O build não instala drivers. O aplicativo não baixa um driver de terceiros,
não muda os dispositivos padrão, não instala certificados e não modifica a
política de boot. O pacote do driver exige a autorização administrativa normal
do sistema. No macOS, o bundle HAL fica em `/Library/Audio/Plug-Ins/HAL`.
No Windows, o instalador usa SetupAPI para criar a instância ROOT; apenas
adicionar um INF com PnPUtil não cria essa instância.

O instalador Windows aceita `install --inf <caminho absoluto de BabelAudio.inf>`,
`remove --inf <mesmo caminho>` e `list`. Ele confere classe MEDIA, fabricante,
serviço, arquitetura, hardware ID e assinatura do catálogo. Uma atualização
preserva um driver mais recente; não força downgrade. Falha ao instalar uma
instância nova tenta remover essa instância. A remoção confere a identidade
instalada e usa o nome OEM informado pelo Windows, nunca curingas. Um pacote
em uso pode exigir reinício e uma nova execução da remoção com o mesmo INF.

Para incluir os pacotes junto ao aplicativo, use `drivers/macos` ou
`drivers/windows` ao lado do executável. No macOS um app bundle também pode
usar `Contents/Resources/drivers/macos`. `babel setup` informa o pacote encontrado
ou como prepará-lo; não informa sucesso de instalação sem ter instalado.
`babel uninstall` informa o helper correspondente. A interface mantém o guia
de instalação do sistema e a atualização da lista de dispositivos.

Uma versão pública Windows precisa cumprir a política de assinatura de drivers
da Microsoft. O pacote macOS de distribuição precisa de assinatura Developer ID
e do fluxo de notarização aplicável. Certificados, contas de desenvolvedor e
aprovação dos fornecedores não são gerados pelo código do projeto.
Fontes oficiais: [política Windows de assinatura](https://learn.microsoft.com/en-us/windows-hardware/drivers/install/kernel-mode-code-signing-policy--windows-vista-and-later-),
[assinatura macOS](https://developer.apple.com/documentation/xcode/creating-distribution-signed-code-for-the-mac/),
[notarização](https://developer.apple.com/documentation/security/customizing-the-notarization-workflow).

## Verificação do transporte instalado

O exemplo Rust `examples/native_driver_smoke.rs` abre somente IDs explícitos,
exige nomes originais Babel e a opção `--confirm-virtual-devices`. Feche o Babel
e outros aplicativos de áudio antes de usá-lo, para não somar áudio externo
ou ativar roteamento físico. Não selecione microfone ou alto-falante reais.

1. Compile no sistema de destino com `cargo build --release --example native_driver_smoke --locked`.
2. Execute `babel devices` e copie os quatro IDs, respeitando `input:`/`output:`.
3. Execute o teste com os IDs exatos, entre aspas:

```text
native_driver_smoke --confirm-virtual-devices \
  --microphone-render "output:<ID da ponta de reprodução do mic>" \
  --microphone-capture "input:<ID da ponta de captura do mic>" \
  --speaker-render "output:<ID da ponta de reprodução da saída>" \
  --speaker-capture "input:<ID da ponta de captura da saída>"
```

No PowerShell use uma linha só ou crases para continuar linhas, e acrescente
`.exe` ao executável. O teste envia quatro tons sintéticos distintos, um por
canal, durante três segundos. Verifica sinal esperado, ausência de troca de
canais, mistura entre cabos, erros de callback e saturação das filas. Imprime
um relatório JSON e fecha os streams. Não grava PCM nem usa provedores de IA.
Um resultado positivo cobre transporte e isolamento nessa execução; latência,
permissões, interrupções, uso por múltiplos aplicativos e estabilidade prolongada
continuam exigindo os cenários de `docs/testing.md`.

## Referências e licenças

O código Babel original e seus núcleos Rust usam MIT. A adaptação Windows usa
o SimpleAudioSample oficial sob **MS-PL**; sua licença e a revisão/hash de cada
arquivo estão incluídos em `native/windows`. Não incorpora VB-CABLE nem BlackHole.
Esses drivers podem continuar sendo instalados separadamente como alternativas,
com suas licenças próprias. O Linux continua usando PulseAudio/PipeWire-pulse.

- [Apple: criação de AudioServerPlugIn](https://developer.apple.com/documentation/coreaudio/creating-an-audio-server-driver-plug-in).
- [Microsoft: SimpleAudioSample](https://github.com/microsoft/Windows-driver-samples/tree/main/audio/simpleaudiosample).
- [Microsoft: miniports de áudio](https://learn.microsoft.com/en-us/windows-hardware/drivers/audio/miniport-driver-types-by-operating-system).
- [SetupAPI: verificação do INF pelo catálogo](https://learn.microsoft.com/en-us/windows/win32/api/setupapi/nf-setupapi-setupverifyinffilew).
