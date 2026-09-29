# Dispositivos virtuais e roteamento por sistema

O processo de tradução é Rust em espaço de usuário. No Linux, o Babel cria os
dispositivos selecionáveis por meio do servidor de áudio existente. No macOS e
Windows, o backend usa CoreAudio/WASAPI via CPAL e se conecta aos dois percursos
do **driver nativo Babel**. O código-fonte e os scripts de compilação estão em
`native/macos` e `native/windows`; BlackHole e VB-CABLE não são dependências desses
drivers. **Os pacotes de distribuição assinados e a validação dos drivers em
hardware macOS/Windows ainda estão pendentes.** Consulte o
[guia dos drivers nativos](native-drivers.md) para compilação e instalação explícita.

O Babel identifica o sistema operacional em que seu processo está executando.
O painel e a bandeja mostram esse sistema, independentemente do navegador usado
para abrir o painel. A identificação não usa o user-agent nem a preferência de
idioma. O painel mostra as instruções, nomes de dispositivos e ações disponíveis
para esse sistema: criação e remoção pelo Babel no Linux; orientação para instalar
o pacote Babel e selecionar seus dispositivos no macOS e Windows. **Atualizar dispositivos** está disponível
nos três sistemas.

A ajuda de dispositivos aberta pelo painel em `/help/platforms` contém apenas
as instruções do sistema atual. A versão completa
fica em `/help/platforms/all` e neste documento. O guia de compilação e pacotes
dos drivers próprios abre em `/help/native-drivers`. Isso permite consultar outros
sistemas de forma explícita, sem misturar seus passos na configuração atual.

Os dois percursos precisam ser independentes:

```text
Microfone físico → captura Babel → tradução → cabo M → microfone da chamada
Saída da chamada → cabo S → captura Babel → tradução → fone/alto-falante físico
```

Selecione dispositivos físicos explícitos no Babel antes de iniciar. Não use o
mesmo cabo nas duas direções: isso pode alimentar novamente a tradução com sua
própria saída. Fones reduzem o retorno acústico entre o alto-falante e o mic real.
O Babel não muda os dispositivos padrão globais do sistema.

## Linux: PipeWire com pipewire-pulse ou PulseAudio

Dependências de execução: `pactl`, `parec` e `pacat`, normalmente fornecidos por
`pulseaudio-utils` (Debian/Ubuntu) ou pelo pacote de ferramentas PulseAudio da
distribuição. O servidor precisa estar acessível na sessão do usuário; não execute
o Babel com `sudo`. ALSA puro sem servidor PulseAudio/pipewire-pulse não é suficiente
para este backend.

A bandeja Linux usa StatusNotifier/AppIndicator. Se o serviço do desktop estiver
ausente durante o login ou bloqueio da tela, o Babel mantém painel e áudio
independentes da bandeja e tenta registrá-la novamente a cada três segundos.
O ícone aparece quando o desktop disponibiliza esse suporte, sem reiniciar o
Babel. Em GNOME, o suporte AppIndicator precisa estar instalado e habilitado.
O programa não desbloqueia a tela nem altera extensões automaticamente.

A ação **criar dispositivos virtuais** carrega módulos do servidor em execução:

| ID | Tipo | Uso |
|---|---|---|
| `babel_microphone` | entrada selecionável, “Babel_Microphone” | Microfone no Zoom/Meet/Discord/etc. |
| `babel_mic_bus` | saída interna, “Babel_Microphone_Bus” | Destino de reprodução da rota de microfone do Babel |
| `babel_speaker` | saída selecionável, “Babel_Speaker” | Alto-falante no aplicativo da chamada |
| `babel_speaker.monitor` | entrada de monitor | Origem de captura da rota de saída do Babel |

O `babel_mic_bus` é um `module-null-sink` mono; `babel_microphone` é um
`module-remap-source` ligado ao monitor desse sink. O `babel_speaker` é outro
`module-null-sink`, estéreo. A captura entregue à IA é mono. O servidor faz a
conversão entre as taxas dos dispositivos e os fluxos PCM16 de cada rota.

A criação é idempotente e recupera módulos parcialmente criados. Um marcador de
propriedade identifica os módulos do Babel; a remoção valida novamente o marcador,
tipo e nome antes de descarregar cada módulo. Dispositivos homônimos de terceiros
causam um erro, em vez de serem substituídos. Os módulos são da sessão atual:
depois de reiniciar o servidor de áudio, crie-os novamente. Encerrar uma tradução
não remove os dispositivos, para não quebrar a seleção da aplicação de chamada.

Se não aparecerem imediatamente no navegador/aplicativo, atualize a lista de
dispositivos ou reabra suas configurações de áudio. Escolha a saída virtual no
aplicativo específico. Aplicativos que só aceitam a saída padrão exigem a escolha
manual do usuário no sistema.

O Babel só abre cada rota quando um aplicativo usa seu dispositivo virtual:
reprodução em `babel_speaker` ativa a saída; captura em `babel_microphone` ativa
o microfone. Selecionar o dispositivo e manter o aplicativo pausado pode mostrar
**Aguardando aplicativo**. Trocar a saída do aplicativo para o alto-falante real
fecha a rota de saída do Babel; o microfone continua independente. Voltar ao
virtual retoma a rota sem encerrar a sessão nem criar novos arquivos.
O controle segue os fluxos efetivamente conectados: mudar apenas o padrão do
sistema não desativa um aplicativo que tenha escolhido explicitamente o Babel.

Em PipeWire, os streams do Babel usam `node.dont-move`, `node.dont-reconnect` e
`node.dont-fallback` para evitar que uma troca de padrão os desvie para outro
dispositivo. A seleção do físico pela bandeja continua funcionando: o Babel
fecha o stream anterior e abre outro no destino escolhido. Em PulseAudio puro,
o monitor detecta desvio do destino e suspende a rota para reabri-la corretamente;
as propriedades específicas do PipeWire não são uma garantia no PulseAudio.
Se o servidor ficar inacessível, as rotas são suspensas e o erro aparece no painel.

Referências oficiais: [módulos PulseAudio](https://wiki.freedesktop.org/www/Software/PulseAudio/Documentation/User/Modules/),
[null sink no PipeWire](https://docs.pipewire.org/page_pulse_module_null_sink.html),
[remap source no PipeWire](https://docs.pipewire.org/page_pulse_module_remap_source.html).
As propriedades de vínculo estão descritas na
[política oficial do WirePlumber](https://pipewire.pages.freedesktop.org/wireplumber/policies/linking.html).

## macOS: driver Babel com dois dispositivos duplex

O pacote **BabelAudio.pkg** instala o AudioServerPlugIn próprio, com dois
percursos separados: **Babel Microphone** e **Babel Speaker**. Cada dispositivo
tem um lado de entrada e um de saída. O código e as instruções de compilação
ficam em [native/macos](../native/macos/README.md); a distribuição usa
`drivers/macos/BabelAudio.pkg` ao lado do aplicativo. Há código-fonte e build,
mas não há um pacote assinado e validado em hardware disponibilizado por este
repositório neste momento. Consulte [preparação dos drivers](native-drivers.md).

O monitor de uso do Babel exige **macOS 14.2 ou posterior**. Instale o pacote
explicitamente e siga a autorização do sistema e as instruções de reinício do
pacote. O painel não executa instaladores nem solicita elevação. Os dispositivos
aparecem nas configurações de som e em Configuração de Áudio e MIDI; não são
aplicativos na pasta Aplicativos. Use **Atualizar dispositivos** após instalar.

Autorize o microfone para o aplicativo/terminal que executa o Babel em
Configurações do Sistema → Privacidade e Segurança → Microfone. A disponibilidade
de captura depende dessa autorização. Reabra o aplicativo se o macOS solicitar.

| Campo/aplicativo | Dispositivo |
|---|---|
| Babel, captura do microfone | seu microfone físico |
| Babel, reprodução do microfone traduzido | saída **Babel Microphone** |
| Aplicativo da chamada, microfone | entrada **Babel Microphone** |
| Aplicativo da chamada, alto-falante | saída **Babel Speaker** |
| Babel, captura da saída para tradução | entrada **Babel Speaker** |
| Babel, reprodução da saída traduzida | seu fone/alto-falante físico |

Os UIDs fixos são `org.babel.audio.microphone.v1` e
`org.babel.audio.speaker.v1`. A seleção usa o UID do dispositivo e a direção;
renomear uma descrição visível não cria outro cabo. Use os IDs mostrados pela
lista do Babel. Os dispositivos próprios oferecem estéreo a 48 kHz; o backend
converte para o formato do percurso de tradução. Não é necessário criar um
dispositivo agregado ou Multi-Output.

O monitor CoreAudio consulta processos externos que usam o **UID selecionado**,
distinguindo captura e reprodução e excluindo o próprio Babel. Capturar Babel
Microphone em outro aplicativo abre a rota do microfone físico; reproduzir em
Babel Speaker abre a rota para os fones. Mudar o padrão global não encerra um
aplicativo conectado explicitamente ao dispositivo. Quando o uso termina, a
rota mostra **Aguardando aplicativo**, fecha seus streams e descarta o áudio
pendente. A outra direção e os arquivos da sessão continuam.

Selecione os dispositivos diretamente nos aplicativos: o monitor não expande
automaticamente Aggregate/Multi-Output. Sistemas anteriores a 14.2, APIs
indisponíveis e erros de consulta mantêm as rotas afetadas fechadas com diagnóstico.
A consulta de processos é periódica, portanto a suspensão não é instantânea.
A compilação e os testes do núcleo do driver não substituem a validação em uma
máquina macOS real, inclusive das permissões e do uso por aplicativos de chamada.

A remoção do driver Babel segue o `uninstall.sh` do pacote, acionado explicitamente
com a autorização necessária. `babel setup` e `babel uninstall` indicam o pacote
ou helper local quando encontrado; não executam esses arquivos nem afirmam ter
instalado/removido dispositivos.

**Alternativa opcional:** duas instalações independentes de BlackHole, por exemplo
2ch e 16ch, continuam compatíveis. Nesse caso, substitua Babel Microphone pelo
BlackHole 2ch e Babel Speaker pelo BlackHole 16ch na tabela. Os canais 1/2 são
utilizados; aplicativos que não aceitem 16 canais precisam de outro loopback
independente compatível. Obtenha e instale os pacotes pelo
[projeto BlackHole](https://github.com/ExistentialAudio/BlackHole), observando sua
[licença e termos de integração](https://github.com/ExistentialAudio/BlackHole#can-i-integrate-blackhole-into-my-app).
O Babel não redistribui BlackHole, e seu driver próprio dispensa essa alternativa.

Referências oficiais: [dispositivos de um processo CoreAudio](https://developer.apple.com/documentation/coreaudio/kaudioprocesspropertydevices),
[exemplo Apple com a API de processos, macOS 14.2+](https://developer.apple.com/documentation/coreaudio/capturing-system-audio-with-core-audio-taps).
O Babel consulta processos/dispositivos, sem criar taps para capturar áudio global.

## Windows: driver Babel com dois pares independentes

O pacote **BabelAudio** fornece quatro endpoints WASAPI que formam dois cabos
independentes. O código do driver WaveRT e os scripts de compilação ficam em
[native/windows](../native/windows/README.md). O INF prevê Windows 10 build
19041 ou posterior. O pacote precisa corresponder à arquitetura do sistema.
**Ainda é necessário produzir o pacote de distribuição assinado e validar a
instalação, os fluxos e a remoção em Windows real.** O código-fonte não é um
instalador já aprovado para produção.

A pasta de distribuição `drivers/windows`, ao lado do executável Babel, reúne
`BabelAudio.inf`, `BabelAudio.sys`, o catálogo e os helpers de instalação/remoção.
Siga [o guia dos drivers](native-drivers.md) para obter ou compilar o pacote e
instalá-lo explicitamente com autorização de administrador. O painel só mostra
as instruções e mantém ocultas as ações de criação/remoção usadas pelo backend
Linux. `babel setup` e `babel uninstall` indicam os arquivos disponíveis, sem
executar scripts, elevar permissões ou afirmar sucesso de instalação.

| Campo/aplicativo | Dispositivo |
|---|---|
| Babel, captura do microfone | seu microfone físico |
| Babel, reprodução do microfone traduzido | **Babel Microphone Feed** (reprodução) |
| Aplicativo da chamada, microfone | **Babel Microphone** (gravação) |
| Aplicativo da chamada, alto-falante | **Babel Speaker** (reprodução) |
| Babel, captura da saída para tradução | **Babel Speaker Monitor** (gravação) |
| Babel, reprodução da saída traduzida | seu fone/alto-falante físico |

Autorize o acesso ao microfone para aplicativos desktop nas configurações de
privacidade do Windows. O Babel usa a configuração compartilhada do dispositivo
via WASAPI. Não ative “Escutar este dispositivo” nos virtuais: isso criaria uma
segunda rota fora do controle do Babel. Use **Atualizar dispositivos** depois da
instalação e selecione as pontas da tabela.

O monitor de sessões WASAPI verifica a ponta usada pelo aplicativo externo:
captura em **Babel Microphone** libera a rota do microfone; reprodução em
**Babel Speaker** libera a rota de saída. As próprias sessões do Babel são
excluídas. Cada direção fica em **Aguardando aplicativo** quando sua ponta não
está em uso, mesmo que a outra direção ou a sessão de gravação continuem ativas.

O pareamento verifica a descrição de cada endpoint fornecida pelo driver e a
identidade de interface **Babel Audio v1**. Os IDs WASAPI são preservados como
identidades opacas; o nome amigável renomeável não determina o par. Pontas
ausentes/ambíguas ou uma falha de consulta mantêm a direção afetada fechada com
diagnóstico. Selecionar os dois lados do mesmo cabo para as duas rotas bloqueia
ambas. Não há fallback para o dispositivo padrão.

O monitor combina enumerações periódicas com eventos das sessões conhecidas.
A Microsoft informa que a enumeração pode não incluir todas as sessões recém
criadas; nesse caso o Babel pode permanecer em espera até conseguir observá-las.
Esta implementação cobre WASAPI compartilhado. Modo exclusivo, ASIO e Kernel
Streaming não foram validados. A compilação cruzada do aplicativo verifica os
tipos/APIs, mas não comprova o funcionamento do driver carregado no Windows.

**Alternativa opcional:** dois pares independentes VB-CABLE ou CABLE-A/B/C/D
continuam reconhecidos. Com CABLE-A e CABLE-B, substitua as quatro pontas Babel
da tabela por CABLE-A Input, CABLE-A Output, CABLE-B Input e CABLE-B Output,
respectivamente. Um único cabo não fornece dois percursos independentes.
Obtenha drivers e licenças diretamente da [VB-Audio](https://vb-audio.com/Cable/)
e consulte os [termos de distribuição](https://vb-audio.com/Services/licensing.htm).
O driver Babel próprio não depende desses pacotes.

Referência oficial: [limites da enumeração de sessões WASAPI](https://learn.microsoft.com/en-us/windows/win32/api/audiopolicy/nf-audiopolicy-iaudiosessionmanager2-getsessionenumerator).

## Limites de desempenho e validação

A suspensão por uso do dispositivo virtual é independente por direção nos
três backends. Ausência de uso fecha os streams da rota, descarta suas filas e
encerra o processamento correspondente; a retomada cria um novo percurso, sem
reproduzir respostas antigas. O nome/ID da sessão e seus writers permanecem
os mesmos. A detecção no macOS e Windows tem os requisitos e limites descritos
acima. Compilar o monitor não comprova seu comportamento com todos os drivers.

- Os callbacks nativos só convertem/misturam amostras, operam filas atômicas com
  capacidade fixa e atualizam contadores. Não fazem rede, alocação, espera,
  bloqueio de mutex ou logging. Ressampling e criação de frames ficam nos workers.
- O ressampling nativo usa FIR sinc de 64 coeficientes e 512 fases com filtro
  anti-aliasing. A taxa nativa do dispositivo é preservada; PCM16 mono entra e
  sai da camada de tradução na taxa configurada pelo provider.
- No Linux, dois clientes persistentes por rota (`parec`/`pacat`) transportam
  PCM bruto. Não há um processo novo por frame. Os buffers e a rede estão fora
  do callback do servidor; é uma escolha de integração simples, não a latência
  mínima possível de uma implementação PipeWire nativa.
- Filas são limitadas. Na captura, saturação descarta frames e incrementa o
  contador; o objetivo é não acumular indefinidamente fala antiga. A reprodução
  usa o limite configurado pela aplicação. Interrupções incrementam uma geração:
  áudio antigo é ignorado mesmo quando a fila está cheia. No Linux, o cliente de
  reprodução reinicia para eliminar o áudio pendente no servidor; no backend
  nativo, a geração também é checada diretamente no callback.
- Se a saída deixa de consumir amostras, a escrita falha após o limite da fila
  (ou da latência, o maior dos dois), mais 500 ms, e encerra a rota com erro.
  Isso também detecta um último frame bloqueado quando o provider não envia
  mais áudio para provocar saturação da fila.
- `latency_ms` é uma solicitação/limite do buffer do Babel. No backend nativo o
  tamanho do callback é negociado pelo CPAL/sistema, não garantido pelo campo.
  No Linux ele é passado ao cliente PulseAudio. Latência percebida inclui rede,
  modelo, detecção de fim de fala, tradução e buffers do hardware. Não há garantia
  de tradução simultânea sem atraso nem benchmark de produção neste repositório.
- Os IDs nativos incluem a direção e a identidade persistente do dispositivo:
  UID no macOS e ID de endpoint no Windows. Reordenar a lista não muda a seleção.
  Configurações antigas que continham índice/nome são aceitas apenas se o nome
  identificar um único dispositivo da direção correta; o índice antigo não é
  usado para escolher outro dispositivo.
- É possível trocar o físico durante a sessão. Se o dispositivo selecionado
  desconectar, o Babel descarta o backlog e tenta reabrir a mesma identidade a
  cada três segundos, sem escolher o padrão do sistema. É possível selecionar
  outro físico durante essa espera. A troca/recuperação preserva a sessão,
  transcrição, writer e provider; frames pendentes da rota física anterior são
  descartados. Captura nativa sem callbacks por dois segundos sinaliza falha
  para iniciar essa recuperação; silêncio com callbacks continua sendo áudio
  válido. O comportamento físico ainda precisa ser verificado em cada SO.
- Cancelar o worker solicita o fechamento nativo, mas não consegue interromper
  uma chamada travada dentro do sistema/driver. O registro por identidade e
  direção mantém o endpoint reservado até sua liberação real, impedindo abrir
  outro stream do Babel para o mesmo endpoint enquanto o anterior persiste.
  Nessa situação, a recuperação pode continuar bloqueada e exigir recuperação
  do driver/processo; o status da tarefa não é prova de fechamento físico.
- `underruns` conta callbacks nativos que precisaram inserir silêncio, inclusive
  quando a IA ainda não produziu fala. PulseAudio não fornece esse contador pelo
  transporte `pacat`, portanto ele permanece zero nesse backend.

Teste o percurso com um provider local de loopback antes de usar a nuvem. A
compilação cruzada confirma tipos e APIs, mas não substitui testes de dispositivo,
permissões, suspensão/retomada e desconexão em máquinas macOS e Windows reais.

Há também um teste Linux real, ignorado na suíte normal. Ele requer os clientes
PulseAudio no `PATH` e uma sessão de áudio acessível, recusa executar se já houver
dispositivos Babel, cria os endpoints, testa tom nas duas rotas, interrupção com
fila cheia e preservação dos padrões, e remove os endpoints no final:

```sh
cargo test --lib live_virtual_routes_idempotence_interruption_and_cleanup -- --ignored
```
