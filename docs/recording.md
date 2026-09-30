# Gravação original da sessão

A gravação de áudio é opcional e vem desativada. Quando ativada, gera **um único
arquivo WAV por sessão**, misturando as entradas originais selecionadas:

- **Microfone:** voz capturada antes da tradução.
- **Áudio recebido:** áudio capturado da saída virtual antes da tradução.

As duas entradas compartilham a mesma linha do tempo. Vozes simultâneas aparecem
sobrepostas no arquivo; intervalos sem áudio são preservados como silêncio.
As faixas não são concatenadas uma depois da outra e o arquivo não contém a voz
sintetizada pelo tradutor.

## Configuração

```toml
[recording]
enabled = false
microphone = true
speaker = true
directory = "recordings"

[files]
# Escolha e adapte uma base absoluta para seu sistema:
# base_path = '/home/ana/Babel'
# base_path = '/Users/ana/Babel'
# base_path = 'C:\Users\Ana\Babel'
name_pattern = "{date}-{time}-{session}-{id}"
```

`babel init` gera a base na pasta `Babel` dentro da pasta pessoal do usuário.
Um TOML existente sem `base_path`, como o trecho acima se não for preenchido,
é tratado como legado: sua base é fixada na pasta do próprio arquivo de configuração.

Ative a gravação na página **Gravação** quando quiser guardar áudio. Escolha as entradas e a
pasta antes de iniciar a sessão. A gravação não é necessária para traduzir.
Ela também funciona sozinha: desligue a tradução das duas direções e habilite a
gravação das fontes desejadas. `microphone.enabled` e `speaker.enabled` controlam
somente a tradução e não limitam as fontes do gravador. Uma sessão só de gravação
passa o áudio original e não usa IA. **Encerrar sessão** finaliza o WAV e mantém
o encaminhamento original enquanto o Babel continuar aberto.
O nome da sessão e o padrão de nomes servem tanto para áudio quanto para texto.

O áudio é configurado em **Gravação** e o texto em **Transcrição**, cada um com
suas próprias origens e pasta. As duas páginas têm o atalho **Pasta base e nomes
de arquivos** para os ajustes comuns. As conexões dos dispositivos ficam em
**Roteamento**; idiomas, provedores e biblioteca de vozes ficam em **Tradução e vozes**.

Em **Ajustes → Arquivos da sessão → Pasta base**, defina a pasta comum dos
arquivos. Com `base_path = "/home/ana/Babel"` e `directory = "recordings"`, o
WAV fica em `/home/ana/Babel/recordings`. A base precisa ser absoluta; configurações
novas usam `Babel` dentro da pasta pessoal (`HOME` ou `USERPROFILE`), sem depender
do diretório de execução. Uma pasta `directory` relativa usa essa base, e uma
pasta `directory` absoluta usa seu próprio destino.
O painel mostra os caminhos completos antes de salvar. A prévia não cria pastas
nem verifica permissões; elas são verificadas ao criar os arquivos no início da
sessão. Alterar a base não move arquivos existentes. Bases relativas antigas são
convertidas uma vez contra a pasta do TOML e salvas como absolutas. Se o diretório
de execução antigo era diferente, confira o destino antes de gravar. Veja as
[regras e exemplos para cada sistema](configuration.md#pasta-base-e-destinos).

Tokens do padrão:

| Token | Conteúdo |
| --- | --- |
| `{date}` | Data de início da sessão em UTC (`AAAAMMDD`). |
| `{time}` | Horário de início em UTC (`HHMMSS`). |
| `{session}` | Nome da sessão convertido em identificador de até 64 bytes. |
| `{id}` | Identificador único da sessão. |

A mesma base é usada para o **único WAV misturado** e o **único TXT com as
transcrições originais das duas entradas**, quando ambos estão habilitados.
As extensões são adicionadas pelo aplicativo, nas pastas configuradas para cada
tipo. O token `{id}` é obrigatório para reduzir colisões entre sessões. Arquivos existentes
nunca são sobrescritos: uma colisão causa erro explícito.

## Incluir áudio anterior ao início

O botão **Iniciar sessão**, o início pela bandeja e `babel run` na CLI sempre
começam no áudio atual, sem incluir o histórico. A API também começa sem
histórico quando `history_seconds` é omitido ou vale zero. Para aproveitar
áudio recente, abra **Opções avançadas de início** ao lado do botão, marque
**Incluir histórico recente** e informe quantos minutos deseja incluir. O
padrão é dez minutos, limitado à capacidade configurada. A opção vem desmarcada
e volta a ficar desmarcada após cada início bem-sucedido.

O painel mostra quanto áudio existe para o microfone e para a saída recebida.
Só entram no WAV as origens selecionadas em **Gravação**; as seleções de
**Transcrição** são independentes. Se o histórico disponível for menor que o
intervalo solicitado, o Babel inclui somente o que ainda está disponível. Sem
histórico em nenhuma origem selecionada, a opção fica indisponível. O histórico
é colocado antes do áudio ao vivo no mesmo WAV, preservando a sobreposição das
origens. Ele não é reproduzido nos dispositivos nem enviado à tradução.

Em **Ajustes → Histórico de áudio recente**, controle a retenção em memória e
sua capacidade: dez minutos por padrão, de um segundo a sessenta minutos. O
atalho **Ajustar retenção do histórico** abre esses controles. Salve para aplicar.
Reduzir a capacidade descarta a parte mais antiga; desativar apaga o histórico.
Aumentar a capacidade não recupera áudio já descartado.

A retenção acompanha as rotas ativas. O histórico do microfone se forma quando
Babel é o microfone padrão do sistema ou um aplicativo usa seu microfone virtual;
o da saída só se forma enquanto um aplicativo envia áudio ao Babel. Escolher
o microfone físico como padrão pausa a captura do microfone se nenhum aplicativo
ainda usa Babel. A retenção não cria arquivos nem envia esse histórico para
transcrição antes de você
incluí-lo explicitamente em uma sessão. O áudio continua na memória ao iniciar
ou encerrar sessões e durante trocas de dispositivos; lacunas de captura não
são recuperáveis. Fechar o Babel perde todo o histórico. A mesma opção funciona
no Linux, macOS e Windows.

```toml
[history]
enabled = true
duration_secs = 600
```

O PCM ocupa até 38,4 MB para dez minutos com as duas origens (mono PCM16 a
16 kHz), além dos metadados. Durante a inclusão, o trecho selecionado é
compartilhado com os gravadores/reconhecedores, sem copiar todo o áudio. Se o
histórico continuar sendo renovado enquanto a inclusão é processada, esse
trecho pode manter temporariamente mais uma janela de PCM em memória. Ele é
liberado assim que os trabalhos que o utilizam terminam.

## Formato e volume

O WAV usa **PCM16 mono a 16 kHz**, sem compressão. É o áudio original que entra no
pipeline do Babel, já convertido pelo backend de captura para esse formato;
não é uma cópia multicanal em 48 kHz do hardware. A gravação ocorre antes de
tradução, sintetização e ganho de saída.

Com as duas entradas ativas, cada uma contribui com ganho de 0,5. Isso deixa
espaço para a soma e evita distorção quando ambas atingem o volume máximo.
Com apenas uma entrada ativa para gravação, o ganho é 1. O arquivo misturado
não permite separar perfeitamente as duas vozes depois; para essa finalidade
seria necessário outro formato de gravação com canais separados.

O consumo em disco é aproximadamente **115 MB por hora**. O limite do contêiner
WAV RIFF é de aproximadamente 37 horas neste formato. Ao atingir o limite, a
sessão retorna um erro em vez de truncar áudio ou criar outro arquivo sem aviso.

## Tempo, troca de dispositivo e continuidade

O gravador usa um relógio monotônico comum à sessão. Cada quadro é posicionado
pelo horário de conclusão da captura menos a duração de suas amostras.
Pequenas variações de agendamento de até 50 ms são absorvidas pela continuidade
de cada entrada; descontinuidades maiores geram intervalos de silêncio.

Uma troca de dispositivo mantém o arquivo da sessão. O intervalo necessário
para fechar e abrir a captura pode aparecer como silêncio quando a nova entrada
retoma. A sincronização é estimada a partir dos horários do pipeline, não de
relógios de hardware compartilhados: latências próprias dos dispositivos podem
introduzir deslocamentos entre as duas fontes.

O mixer mantém uma janela limitada de dois segundos para receber quadros das
duas entradas antes de consolidar sua soma. Ele reserva no máximo três segundos
de amostras de mistura, incluindo espaço para um quadro recebido, e escreve
intervalos longos em blocos limitados. A fila de entrada também é limitada.
Uma entrada excessivamente atrasada ou uma falha de escrita é comunicada ao
supervisor da sessão, evitando perda de áudio silenciosa.

## Finalização e privacidade

Ao parar normalmente, o Babel deixa o gravador consumir os quadros já enfileirados,
escreve a parte final e atualiza o cabeçalho WAV antes de encerrar. Cabeçalhos
intermediários são atualizados periodicamente para os dados já consolidados.
Uma queda de energia, encerramento forçado ou falha de disco pode perder os
últimos segundos; uma gravação interrompida não tem a mesma garantia de uma
parada concluída.

Os arquivos usam criação exclusiva e permissão `0600` em Unix: leitura e escrita
somente pelo usuário. No Windows, seguem as permissões da pasta escolhida.
O gravador grava localmente e não faz upload. Isso não altera o envio de áudio
que a própria tradução exige quando um provider remoto está selecionado.

## Testes

```sh
cargo test --lib recording:: -- --nocapture
```

Os testes usam somente PCM sintético e pastas temporárias. Cobrem mistura
sobreposta em arquivo único, headroom, entrada única, silêncio, compensação de
jitter, intervalos longos sem crescimento ilimitado da memória, atraso excessivo,
finalização após erro, drenagem no encerramento, cabeçalhos válidos, privacidade
e recusa de sobrescrita. Nenhum teste captura voz pessoal ou ativa gravação na
configuração real.

No Linux com PulseAudio/`pipewire-pulse`, há também um teste completo opcional:

```sh
cargo test --test recorded_session -- --ignored --nocapture
```

Ele cria quatro sinks nulos temporários isolados, passa dois tons pelo Controller
e verifica um único WAV com ambas as frequências sobrepostas. A execução real
gerou um arquivo válido de 3,53 s, encerrou sem erro e removeu somente os módulos
do teste. Os dispositivos Babel já instalados permaneceram intactos. É preciso
ter `pactl`, `parec` e `pacat` no `PATH`; nenhum dispositivo físico é usado.
