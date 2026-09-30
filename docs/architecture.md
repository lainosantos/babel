# Arquitetura, memória e desempenho

## Componentes

```text
Dispositivo de captura
  → backend PulseAudio ou callback CPAL
  → filas limitadas
     ├─ sem sessão → passagem original → reprodução
     └─ sessão ativa → supervisor da faixa
        ├─ tradução ligada → SpeechProvider → PCM16 mono 24 kHz → reprodução
        └─ tradução desligada → passagem original → reprodução

Na sessão, independentemente da tradução:
  captura original → gravador opcional → mistura → um arquivo .wav
  captura original → reconhecimento opcional → um arquivo .txt com origens

Modo opcional de voz:
  texto traduzido em memória → segmentador → TTS em streaming → PCM de reprodução
```

As duas faixas são independentes em termos de provider, idiomas, prompts e voz,
mas compartilham a configuração do transporte de áudio. Cada faixa de nuvem tem
sua própria sessão e contexto; os provedores recebem as origens separadamente.
A flag `enabled` de cada faixa seleciona somente tradução: a rota continua
encaminhando áudio original quando essa flag está desligada. Transcrição e
gravação selecionam suas fontes separadamente. Sem tradução, o adaptador de ASR
produz somente texto original, sem enviar esse áudio ao tradutor ou sintetizador.
A gravação opcional mistura os originais somente no WAV local. Falhas de
processamento, provedor ou escrita encerram a sessão e permanecem no status.
O monitor restaura a passagem original automaticamente após o fechamento dos
streams, verificando esse estado a cada 250 ms. Isso pode produzir uma lacuna.
Falhas dos dispositivos físicos têm recuperação própria: o painel mostra o erro
e permite escolher outro dispositivo pela bandeja sem recriar a sessão de IA.

`SpeechProvider` é a interface assíncrona que recebe quadros de PCM e emite
`ProviderEvent`. Não há rede nem JSON nos callbacks de áudio. Adaptadores novos
podem produzir metadados reais de locutor/tempo em `TranscriptMetadata`; ausência
permanece ausência. O catálogo de capacidades deve acompanhar a implementação e
os modelos efetivamente suportados.

A camada de vozes é separada: enumeração, design, clonagem e síntese pertencem a
`voices`. O adaptador `revoice` solicita texto de saída ao tradutor, descarta o
áudio original sintetizado por ele e reproduz a voz selecionada. O segmentador
prefere pontuação, limita o tamanho de trechos e faz flush por prazo. Sínteses são
ordenadas para preservar a sequência das falas. Isso pode perder contexto de
prosódia entre trechos e acrescenta inferência, rede e custo.
No tradutor local, selecionar um TTS externo pula o Piper: reconhecimento e
tradução entregam texto diretamente ao sintetizador escolhido.

## Inferência local gerenciada

`local_runtime` prepara os providers locais selecionados na configuração salva,
mesmo com a respectiva função desligada. O plano reúne os modelos Whisper
necessários, o tradutor Qwen via llama.cpp e as vozes Piper usadas pelas rotas.
O STT continua separado do tradutor: configuração e processamento próprios,
compartilhando um motor carregado quando o modelo coincide.

O manager verifica os componentes empacotados e baixa pesos ausentes do catálogo
pinado por tamanho/SHA-256. Os motores nativos são processos filhos persistentes;
Whisper e llama.cpp anunciam a porta de loopback já reservada por `bind(0)`.
Piper usa JSON por stdin/stdout, atrás de um gateway HTTP Rust com porta dinâmica.
Somente o snapshot efetivo recebe esses endpoints; o TOML preserva `auto`.

Preparação e progresso aparecem em `EngineStatus.local_runtime`. Iniciar a sessão
aguarda apenas os recursos das funções ativas. Um Whisper já pronto pode atender
STT enquanto componentes locais selecionados, mas inativos, ainda são preparados;
uma falha nessa preparação opcional não invalida o endpoint STT disponível.
Cancelar o início interrompe a espera da sessão. Alterar a seleção reconcilia o
plano, cancela a preparação anterior e encerra os filhos que deixam de pertencer
a ele; sair do Babel encerra os processos gerenciados. Endpoints externos
explícitos permanecem sob responsabilidade do usuário.

Preparar modelos não abre captura para tradução/transcrição nem cria arquivos
de sessão. Depois do primeiro download, inferência integrada funciona offline.
O diretório de pesos é próprio e absoluto, separado dos TXT/WAV; `threads`
controla Whisper/llama.cpp, sem prometer controlar threads internas de Piper.
Veja [modelos locais](local-inference.md) para catálogo, caminhos e limites.

## Sessão, arquivos e troca de dispositivo

O controlador cria a identidade da sessão uma vez. O nome opcional do usuário
fica separado do identificador seguro e do padrão de arquivos. `files.name_pattern`
produz uma base comum para TXT/WAV; `{date}` e `{time}` usam UTC, `{session}` é
uma versão segura do título limitada a 64 bytes e `{id}` é obrigatório. Os writers
acrescentam a extensão e criam arquivos novos, sem sobrescrever sessões anteriores.

Um writer de transcrição recebe as origens pelo mesmo canal limitado e grava
um único TXT. Cada fragmento mantém seus metadados e é identificado por
`[microfone]` ou `[saída recebida]`. A sequência do arquivo é a chegada dos
fragmentos; conexões independentes podem ter atrasos e bases de timestamps
diferentes. A união não fornece ordenação perfeita da fala nem diarização.

O gravador opcional recebe PCM16 mono a 16 kHz antes da tradução e do ganho de
reprodução, mistura as origens selecionadas e mantém um único WAV. Transcrição e
gravação têm seleções de faixas e pastas independentes; ambas ficam desligadas por
padrão. Habilitar um recurso sem qualquer origem selecionada é erro de
configuração. A origem precisa estar selecionada e configurada, mas sua tradução
pode estar desligada. Os writers trabalham fora dos callbacks de áudio.

`EngineStatus.running` representa a sessão de processamento/arquivos; ela pode
conter só gravação, só reconhecimento ou uma combinação com tradução. Sem sessão,
`routing_active` e `routing_error` descrevem a passagem original local. Essa
passagem não envia áudio a provedores nem abre arquivos. Motores locais
selecionados podem ficar preparados independentemente da sessão. `stop()` encerra a sessão e retoma o
original; `shutdown()` encerra também esse roteamento quando o aplicativo sai.
O original usa PCM16 mono a 48 kHz e quadros de 10 ms, inclusive durante uma
sessão com tradução desligada. Cópias para ASR/WAV são convertidas para 16 kHz.
Iniciar/encerrar sessão pode reabrir streams e produzir um breve intervalo, sem
trocar os dispositivos virtuais selecionados pelos outros aplicativos.

No Linux, um monitor de uso fecha cada rota enquanto nenhum aplicativo usa seu
endpoint virtual. Ele acompanha eventos do servidor PulseAudio/pipewire-pulse e
confere snapshots limitados; fluxos internos do Babel e do remapeamento não
contam como consumidores. Uma aplicação com seleção própria continua funcionando
mesmo quando o padrão do sistema é outro dispositivo. O estado `waiting_for_app`
mostra essa espera; `running` continua representando a sessão, enquanto
`routing_active` só fica ativo se alguma direção estiver processando.

Ao deixar de usar o virtual, o supervisor cancela captura, reprodução, provedor e
ativação por voz daquela direção. Fecha os streams e descarta suas filas antes
de reabrir. A próxima ativação cria novas conexões e canais: eventos de áudio ou
transcrição da conexão antiga não entram na nova. Epochs separados por direção
preservam até desativações/reativações rápidas agrupadas pelo canal de controle.
O nome/ID da sessão e os writers TXT/WAV permanecem; a transcrição recebe uma
quebra, e o WAV mantém o relógio da sessão. Essa pausa pode interromper uma frase
em processamento, mas não reproduz a frase atrasada depois de voltar.
Falha de inspeção fecha as rotas e aparece no painel. No Windows, um worker COM
MTA inspeciona as sessões WASAPI do lado oposto de cada cabo Babel (ou VB-Audio opcional), excluindo
o PID do Babel. O pareamento usa IDs de endpoints e metadados do driver; pares
ausentes, ambíguos ou compartilhados entre as duas rotas são recusados. A consulta
periódica é complementada por callbacks de estado das sessões já descobertas.
No macOS 14.2+, um worker consulta os processos CoreAudio a cada 200 ms e cruza
PID, estado e dispositivos por direção; não usa o estado global do dispositivo,
que incluiria o próprio Babel. Sistemas anteriores suspendem as rotas com um
diagnóstico, sem captura contínua como fallback. Essas consultas não capturam
áudio e não executam nos callbacks de áudio. O período de detecção acrescenta
uma pequena janela ao iniciar/parar o roteamento; não é uma barreira instantânea.

As seleções de microfone e saída físicos são atualizadas pela bandeja. A camada
de dispositivo substitui o stream envolvido enquanto mantém provedor, identidade
da sessão e arquivos. O stream pode ficar indisponível durante a troca ou após
uma falha; o erro aparece no painel e uma nova seleção permite recuperação.
Com ou sem sessão, a camada de dispositivo tenta novamente o mesmo endpoint com
erro a cada três segundos. A seleção manual de outro dispositivo aciona a troca
imediatamente. No macOS/Windows, CPAL fornece o UID/ID persistente do sistema;
mudanças na ordem de enumeração não alteram a seleção. Não há substituição pelo
dispositivo padrão. Configurações antigas baseadas em índice só são resolvidas
por nome quando há exatamente um candidato, até serem salvas com o novo ID.
O caminho não conserva um backlog de áudio antigo para reproduzir ao recuperar.
Essa continuidade de sessão não significa continuidade acústica sem lacunas.
Demais alterações de configuração exigem que a sessão esteja encerrada; o áudio
original continua sendo encaminhado nesse estado.

Cada alteração salva incrementa a revisão da configuração. O painel lê um
snapshot atômico com ETag e envia `If-Match` ao salvar ou iniciar; o controlador
confere a revisão sob o mesmo lock da operação. Uma revisão antiga resulta em
HTTP 412. O painel acompanha mudanças externas também durante a sessão: atualiza
ajustes limpos e preserva rascunhos até uma recarga explícita, sem sobrescrever
silenciosamente uma escolha feita na bandeja.

## Garantias e limites de segurança de memória

O aplicativo principal e os núcleos portáveis de transporte dos drivers usam
`#![forbid(unsafe_code)]`. A integração HAL/WDK e o instalador têm fronteiras FFI
separadas, com ponteiros e chamadas ao sistema documentados em `native/`.
No macOS, uma ponte C usa os layouts do SDK Apple; no Windows, C++ fica na
integração WaveRT/PortCls e Rust `no_std` transporta o PCM em armazenamento fixo.
Veja [divisão dos drivers e instalação](native-drivers.md).
Rust verifica a propriedade dos buffers e os acessos no núcleo seguro.
Bibliotecas de rede, áudio, sistema, drivers, firmware, whisper.cpp/llama.cpp/Piper/ONNX e modelos externos não herdam uma
prova de segurança apenas porque o chamador é Rust. Não há afirmação de que todo
o stack ou todos os drivers sejam livres de `unsafe`/C/C++.

Nos backends nativos, callbacks usam filas lock-free previamente alocadas,
conversão de amostras e contadores atômicos. Não fazem alocação, locks de mutex,
rede, escrita de arquivos ou logging. O worker aplica resampling sinc de 64 taps,
com filtro anti-alias, fora do callback. No Linux, `parec`/`pacat` persistentes
fazem I/O e conversão pelo servidor existente; não se inicia um subprocesso por
quadro. `pactl` é usado para administração/listagem, fora do caminho de áudio.

As estruturas de controle usam locks curtos para configuração e estado. O
supervisor nunca precisa desses locks dentro de um callback CPAL. PCM malformado,
taxas inesperadas, JSON excessivo, SSE sem término e respostas HTTP incompatíveis
são rejeitados antes de alimentarem dispositivos.

## Orçamento de buffers

As filas têm limites explícitos. Com defaults por faixa, 200 ms de PCM16 a 16 kHz
correspondem a aproximadamente 6,4 KiB por fila de captura/envio. A fila de áudio
traduzido de 2000 ms a 24 kHz comporta aproximadamente 96 KiB de amostras. Há buffers
adicionais limitados nos backends, eventos de provider, TLS/WebSocket/SSE, pipes e
no próprio sistema operacional. Esses números não são o RSS total do processo.

O parser Gemini limita uma mensagem WebSocket a 512 KiB e uma parte de áudio a 1 s.
Os demais parsers definem limites próprios. As filas carregam valores de tamanho
validado, evitando que um limite por número de mensagens esconda mensagens
arbitrariamente grandes. A síntese transmite quadros de 480 amostras ou menores e
faz pacing pela duração real do PCM, não pelo número de mensagens HTTP.

Captura atrasada é descartada; saturação contínua de texto/reprodução encerra o
fluxo com erro em vez de acumular minutos de atraso. Um contador de geração
invalida o áudio antigo em uma interrupção, inclusive quando a fila está cheia.
Linux reinicia o stream de reprodução para limpar o buffer no servidor; CPAL
ignora amostras de gerações antigas. A interrupção não pode desfazer som que já
chegou fisicamente ao alto-falante.

## O que determina a latência

A latência total inclui captura, agrupamento de quadros, transporte, inferência,
necessidade linguística de contexto, retorno da IA, fila de reprodução e driver.
Uma língua pode exigir esperar pelo fim de uma construção para traduzi-la
corretamente. Os presets locais não eliminam essa necessidade.

Modelos conversacionais podem esperar uma pausa/VAD. O pipeline open source
segmenta a fala antes de executar STT → tradução → TTS. Modelos dedicados permitem
fala contínua, mas latência e disponibilidade dependem da conta, região, rede,
carga do serviço e limites. O modo de voz TTS adiciona outra requisição por trecho.

Não há meta de milissegundos garantida. O smoke de inferência local carrega
modelos reais, mas não mede latência de conversas; benchmarks de serviços de
nuvem dependem de acesso autenticado. Avalie p50/p95/p99 por idioma e hardware, durante uma sessão longa, além da
média. O teste virtual local verifica transporte e funcionamento, não qualidade
semântica, prosódia, diarização ou desempenho de nuvem.

## Falhas, reconexão e privacidade

Conexões e escritas têm deadlines e orçamento de reconexão. Reconectar pode gerar
lacunas: áudio antigo não é reproduzido/reenviado indefinidamente para tentar
recuperar tudo. Gemini conversacional usa resumption quando disponível; tradução
contínua pode abrir uma sessão nova. Uma chave/modelo rejeitado não deve virar um
loop de reconexões interminável.

Configuração salva contém referências de credenciais; chaves temporárias ficam
na memória com zeroização na substituição/drop. Cópias necessárias para cabeçalhos
HTTP/TLS e buffers internos das dependências não são uma garantia de apagamento
criptográfico de toda memória do processo. Erros remotos são sanitizados para
não expor cabeçalhos, tokens, áudio ou texto do usuário.

Áudio de uma faixa com tradução ativa é enviado ao tradutor escolhido. Com voz
externa, texto traduzido também vai ao sintetizador. Com somente transcrição,
o áudio segue apenas ao reconhecedor escolhido; gravação e passagem original
não usam IA. O modo local só evita serviços de nuvem se
os endpoints configurados forem locais. Referências de voz são enviadas quando o
usuário cria o perfil; perfis persistentes ficam na conta do fornecedor. Consulte
políticas/retenção do provedor. O Babel não grava PCM das conversas por padrão.
Um WAV com as origens selecionadas é criado somente quando `recording.enabled`
está ativo; textos originais só são gravados com `transcription.enabled` ativo.
Essas duas opções são independentes e não incluem a fala traduzida.

## Extensão

Para acrescentar outro tradutor, implemente `SpeechProvider`, valide formatos,
limites e endpoints, registre a factory e a configuração específica, publique suas
capacidades e escreva testes de protocolo com servidor simulado. Para outro TTS,
implemente síntese incremental 24 kHz e operações de biblioteca suportadas. Não
reutilize o formato de setup de outro fornecedor apenas porque ambos usam JSON.
Diarização/clonagem automática exigiriam um pipeline próprio com IDs estáveis,
amostras por participante, enrollment autorizado e alinhamento entre original e
tradução; mapear a última voz ou um canal a uma pessoa seria incorreto.
