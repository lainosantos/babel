# Modelos locais integrados

O Babel inicia os providers locais selecionados e gerencia os modelos necessários.
O instalador inclui os motores de inferência para Linux, macOS e Windows. O usuário
não precisa instalar Python, Ollama, CMake, um compilador ou um servidor separado.
Os pesos dos modelos são baixados automaticamente na primeira seleção e ficam em
cache para as próximas execuções.

## Começar pela interface

Para transcrição, abra **Transcrição**, escolha **whisper.cpp** em uma ou nas duas
origens e mantenha **Integrado ao Babel** no perfil. Escolha Whisper Tiny, Base ou
Small e salve os ajustes. O Babel prepara o reconhecedor mesmo antes de iniciar
uma sessão. Ativar a transcrição continua sendo uma escolha independente.

Para tradução, abra **Tradução e vozes**, escolha **Local** na rota desejada e
mantenha os componentes de reconhecimento, tradução e voz em **Integrado ao
Babel**. Salvar prepara Whisper, Qwen e Piper conforme a configuração selecionada.
A opção de voz automática escolhe uma voz disponível para o idioma de destino.

O estado aparece nas duas páginas e em **Ajustes → Modelos locais**:

- **Em espera:** ainda não há uma preparação ativa para a seleção salva.
- **Preparando:** baixando pesos ou carregando os motores; o download mostra
  tamanho recebido e percentual quando o servidor informa o total.
- **Prontos:** os motores selecionados estão disponíveis.
- **Precisam de atenção:** a preparação falhou; os detalhes ajudam a corrigir
  rede, armazenamento, modelo ou instalação. Salvar novamente tenta a preparação.

Iniciar uma sessão aguarda os modelos ficarem prontos. **Cancelar início da
sessão** interrompe essa espera, sem exigir desligar o roteamento original.
A preparação dos modelos que continuam selecionados pode prosseguir. Os ajustes
salvos determinam quais serviços gerenciados são necessários; os processos
pertencem ao Babel e são encerrados por ele ao deixar de serem usados.

Não é necessário iniciar uma sessão para preparar os modelos, mas preparar um
modelo não grava, transcreve ou traduz automaticamente. A captura para essas
funções segue as opções da sessão e o uso do dispositivo virtual correspondente.
Com tradução, transcrição e gravação desligadas, o áudio original segue a rota
física configurada.

## Modelos disponíveis

| Etapa | Catálogo integrado | Padrão e considerações |
|---|---|---|
| Reconhecimento original | Whisper `tiny`, `base`, `small`, multilíngues | `base`; modelos maiores consomem mais memória e processamento |
| Tradução de texto | Qwen `qwen3-0.6b` | Motor llama.cpp incluído; modelo compacto, sem garantia universal de qualidade |
| Síntese | Piper, vozes listadas abaixo | `auto` acompanha o idioma de destino |

Tamanhos aproximados de download, em MB decimais (1 MB = 1.000.000 bytes):

| Arquivo de modelo | Download |
|---|---:|
| Whisper Tiny | 78 MB |
| Whisper Base | 148 MB |
| Whisper Small | 488 MB |
| Qwen3 0.6B Q8 | 639 MB |
| Cada voz Piper | 63–64 MB, mais configuração e licença |

A interface mostra o progresso em MiB (1 MiB = 1.048.576 bytes), por isso o número
exibido difere dessa tabela. O tamanho do download não representa o uso de RAM
durante a inferência. Uma tradução local com Whisper Base, Qwen e duas vozes
precisa de aproximadamente 914 MB em pesos; os motores do instalador e arquivos
temporários ocupam espaço adicional. Pesos já presentes e verificados são
reutilizados, sem novo download a cada sessão.

| Idioma de destino | Voz Piper integrada |
|---|---|
| Inglês | `en_US-lessac-medium` |
| Português | `pt_BR-faber-medium` |
| Espanhol | `es_ES-davefx-medium` |
| Francês | `fr_FR-siwis-medium` |
| Alemão | `de_DE-thorsten-medium` |
| Italiano | `it_IT-paola-medium` |
| Chinês | `zh_CN-huayan-medium` |

O `voice_id` explícito da rota tem prioridade sobre `piper_voice` do perfil. A
voz deve ser compatível com o idioma falado. Se o catálogo integrado não cobrir
seu idioma de destino, escolha uma voz TTS externa compatível ou configure um
servidor próprio. Clonagem, voice design e preservação da identidade original
não são recursos dessas vozes Piper integradas.

Whisper do perfil de tradução e Whisper do STT têm modelos e segmentação
independentes. Duas origens e uma tradução simultânea podem compartilhar motores,
mas ainda precisam processar cada fluxo; isso aumenta a carga. O TXT contém
somente os resultados do STT escolhido, não o texto intermediário da tradução.

## Pasta e processamento

Em **Ajustes → Modelos locais**, defina:

- **Pasta dos modelos:** vazia usa o cache do aplicativo na conta do sistema.
  Para escolher outro disco ou pasta, informe um caminho absoluto. Exemplos:
  `/home/usuario/Babel-models` no Linux, `/Users/usuario/Babel-models` no macOS ou
  `D:\Babel-models` no Windows. `~` e variáveis de ambiente não são expandidos.
- **Threads de CPU para inferência:** de 1 a 64, padrão 4, para Whisper e Qwen.
  Esse ajuste não controla as threads internas do Piper. Um valor maior não
  garante menor latência e pode disputar CPU com os dispositivos de áudio.

Quando o campo fica vazio, o diretório padrão é:

| Sistema do Babel | Pasta padrão dos pesos |
|---|---|
| Linux | `$XDG_DATA_HOME/babel/models`, quando definido como caminho absoluto; senão `$HOME/.local/share/babel/models` |
| macOS | `$HOME/Library/Application Support/Babel/models` |
| Windows | `%LOCALAPPDATA%\Babel\models` |

Esses nomes descrevem as variáveis do ambiente do processo Babel. O campo da UI
não expande essas expressões: para informar uma pasta própria, use o caminho
absoluto completo. Os pesos ficam nos dados locais do aplicativo, não na pasta
de downloads do navegador. A ausência de um diretório de conta válido exige
escolher uma pasta absoluta explicitamente.

A pasta dos modelos é independente da base dos arquivos de sessão TXT/WAV.
Alterá-la não move os arquivos existentes: um modelo ausente no novo local
precisa ser preparado novamente. Reserve espaço para todos os modelos
selecionados; modelos e vozes diferentes têm tamanhos diferentes.

```toml
[local_runtime]
directory = "" # Cache do aplicativo, ou caminho absoluto no sistema anfitrião.
threads = 4

[transcription.providers.whisper]
endpoint = "auto"
model = "base"
api_key_env = ""
segment_ms = 2000
silence_ms = 300
vad_threshold = 0.01
request_timeout_secs = 30

[providers.local]
whisper_endpoint = "auto"
whisper_model = "base"
ollama_endpoint = "auto"
translation_model = "qwen3-0.6b"
piper_endpoint = "auto"
piper_voice = "auto"
segment_ms = 2000
silence_ms = 300
vad_threshold = 0.01
request_timeout_secs = 30
```

A primeira preparação requer acesso à internet para baixar os pesos publicados.
Downloads são verificados contra o catálogo de integridade do Babel antes do uso.
Uma falha não deve ser confundida com um modelo pronto. Depois de preparar os
modelos necessários, a inferência integrada funciona offline. Selecionar outro
modelo ainda ausente requer uma nova preparação com acesso à rede.

## Portas, credenciais e modo externo

`endpoint = "auto"` e os endpoints de componentes com valor `"auto"` significam
processos gerenciados. O Babel escolhe portas disponíveis dinamicamente em
loopback; não confia em um serviço já presente numa porta conhecida. Os
endereços temporários não substituem `auto` no arquivo de configuração. O
reconhecedor integrado não exige uma chave de API do usuário.

**Servidor externo (avançado)** permite manter instalações próprias ou servidores
em outra máquina. Informe a URL completa, incluindo a porta real, quando houver.
Nesse modo, o Babel não instala, inicia, atualiza ou baixa modelos para esse
servidor. O Whisper usa o modelo carregado nele; a escolha Tiny/Base/Small da UI
só controla o motor integrado.

No STT Whisper externo, `api_key_env` é uma referência opcional a uma credencial
Bearer. O segredo pode vir do ambiente do processo ou da chave temporária
aplicada no painel. Não coloque o segredo no TOML. Endereços HTTP sem TLS são
aceitos somente no loopback; servidores remotos precisam de HTTPS. URLs com
credenciais embutidas não são aceitas.

Na tradução externa, o campo legado `ollama_endpoint` escolhe o endereço e
`translation_api` define o protocolo: `ollama` para `/api/chat` ou `openai` para
chat completions compatível. Esse nome legado não implica instalar Ollama no
modo integrado; o runtime gerenciado usa llama.cpp. É possível gerenciar um
componente e usar um endpoint externo em outro, sem acoplar o STT da transcrição.

## Instalação, desenvolvimento e limites

Use um instalador ou pacote do Babel que inclua os runtimes correspondentes ao
sistema e à arquitetura. Copiar apenas o executável Rust de uma compilação de
desenvolvimento não inclui automaticamente as bibliotecas e motores nativos.
Se o pacote estiver incompleto, a UI informa o erro de preparação; atualizar a
instalação completa é preferível a apontar para uma porta arbitrária.

Os motores nativos são processos separados do núcleo Rust. O código Rust do
Babel proíbe `unsafe` próprio, mas isso não torna whisper.cpp, llama.cpp, ONNX ou
outros componentes nativos memory-safe. Eles têm seus próprios contratos,
atualizações e licenças; os pesos das vozes também têm licenças próprias.

O reconhecimento integrado trabalha em segmentos. A tradução local acumula o
tempo de segmentar, reconhecer, traduzir e sintetizar. Tiny reduz o custo, mas
pode sacrificar qualidade; Small pode exigir mais recursos. Não há promessa de
latência fixa nem de desempenho universal de fala contínua em qualquer CPU.
Para reduzir sobrecarga, comece por uma direção, um modelo menor e um número de
threads que deixe CPU disponível para o áudio. O painel preserva erros de
preparação e os limites de fila evitam acumular atraso indefinidamente.

A transcrição Whisper salva o texto original, sem diarização nem timestamps por
palavra. Seus tempos correspondem aos segmentos capturados. Veja o
[guia de transcrição](transcription.md) e o
[guia dos providers](other-providers.md) para limites de cada protocolo.

## Verificação dos pacotes no CI

Além de validar os hashes e executar `--help`, o CI usa
`scripts/test_bundled_inference.py` para carregar os três motores reais na
arquitetura do runner. O teste baixa somente pesos pinados do catálogo, envia
um segundo de silêncio sintético ao Whisper e uma frase fixa ao Qwen, e pede
duas falas ao mesmo processo Piper. Verifica descoberta de porta dinâmica,
respostas JSON, caminhos Unicode de saída e amostras WAV válidas. Nenhum
microfone, dispositivo de áudio ou chave de nuvem é usado.

Esse teste verifica instalação, carregamento e protocolo, sem medir qualidade
linguística, latência durante chamadas reais ou funcionamento dos drivers.
Python é usado apenas pelo teste de desenvolvimento/CI; não é dependência do
Babel instalado. O cache de teste aceita o mesmo formato de nomes e hashes dos
modelos do aplicativo, permitindo reutilizar pesos já verificados.
