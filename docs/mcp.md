# Integrações MCP para comandos por voz

O Babel pode conectar vários servidores MCP registrados nas configurações do agente. O reconhecedor local identifica o nome de ativação e a instrução; o decisor recebe somente o catálogo das integrações habilitadas. A execução confere novamente a ferramenta e seu JSON Schema no servidor antes de chamá-la. Texto devolvido por ferramentas é conteúdo, não uma autorização para executar instruções adicionais.

Nenhuma integração é adicionada automaticamente. Escolha servidores e credenciais que correspondam às ações que deseja permitir ao agente. Desabilitar uma integração impede novas chamadas. A lista `allowed_tools` restringe as ferramentas de um servidor; vazia, permite todo o catálogo anunciado por esse servidor. A seleção pode afetar serviços externos: um servidor de arquivos, por exemplo, pode disponibilizar ferramentas de escrita além de leitura.

Whisper e Needle são os serviços locais de reconhecimento e decisão, separados
dos servidores MCP deste guia. Seus endpoints padrão são `auto`: depois da
instalação explícita, o Babel inicia os helpers em portas escolhidas pelo sistema
operacional e mostra os endpoints efetivos no status de **Comandos**. A pasta
dos serviços fica nessa página; vazia, usa a pasta absoluta do TOML carregado.
Consulte [instalação e operação dos comandos de voz](voice-commands.md).
Esse gerenciamento não instala, autentica nem seleciona integrações MCP.

## Transportes

| Transporte | Configuração | Autenticação | Observações |
|---|---|---|---|
| `stdio` | Executável, argumentos separados, diretório opcional | Variáveis de ambiente, incluindo referências a segredos | Babel inicia o processo diretamente, sem montar um comando de shell |
| `http` | URL completa do endpoint Streamable HTTP, geralmente `/mcp` | Sem autenticação, Bearer, cabeçalhos personalizados ou OAuth | HTTPS obrigatório; HTTP aceito para loopback (`localhost`/`127.0.0.1`) |

O transporte SSE antigo, normalmente exposto em `/sse` com um endpoint separado para mensagens, não é implementado. Use o endpoint Streamable HTTP. Respostas SSE do próprio Streamable HTTP são suportadas.

O cliente usa o SDK Rust oficial `rmcp` 3.5.0 para negociação, `initialize`/`notifications/initialized`, sessões, correlação JSON-RPC e descoberta de ferramentas. A versão preferida de protocolo é `2025-11-25`, mantendo compatibilidade com o ciclo de inicialização amplamente usado. Recursos exclusivos do ciclo sem `initialize` de revisões mais recentes não são prometidos.

## Adicionar uma integração

1. Instale e configure o servidor MCP conforme a documentação dele, ou obtenha a URL remota.
2. Adicione uma integração nas configurações do agente. O `id` é único e estável; o nome é somente para apresentação.
3. Escolha o transporte e preencha seus parâmetros.
4. Configure as referências de credenciais necessárias. Cadastre os valores na área de credenciais ou forneça variáveis de ambiente ao processo Babel.
5. Se for OAuth, salve a integração e use a ação de conectar/autenticar para abrir o login no navegador.
6. Teste a conexão. O teste inicializa o MCP e lista as ferramentas; não executa uma ferramenta de negócio.
7. Restrinja `allowed_tools` quando quiser expor somente parte do catálogo ao agente.

Uma integração mal configurada retorna uma falha visível. Os erros da camada de transporte não reproduzem corpos HTTP, tokens, códigos de autorização ou a saída de erro de subprocessos.

## Exemplo local com stdio

Trecho de `babel.toml`; adapte os caminhos para o pacote MCP instalado. Executável e argumentos são campos separados; não escreva uma linha de shell em `command`.

```toml
[[agent.integrations]]
id = "arquivos"
name = "Arquivos de trabalho"
enabled = true
transport = "stdio"
command = "/usr/bin/node"
args = ["/caminho/servidor-mcp/dist/index.js", "/caminho/pasta-permitida"]
cwd = "/caminho/servidor-mcp"
auth = "none"
timeout_secs = 30
allowed_tools = ["list_directory", "read_text_file"]

[agent.integrations.env]
LOG_LEVEL = "error"

[agent.integrations.secret_env]
SERVICE_API_KEY = "BABEL_ARQUIVOS_SERVICE_KEY"
```

`secret_env` mapeia o nome recebido pelo servidor para o nome da credencial no Babel. O exemplo injeta o segredo de `BABEL_ARQUIVOS_SERVICE_KEY` como `SERVICE_API_KEY` no processo filho. `env` contém valores comuns, persistidos em texto no TOML; coloque segredos em `secret_env`.

No Windows, use um executável real, por exemplo o caminho completo de `node.exe` ou `python.exe`. Um shim `.cmd` de um gerenciador de pacotes pode não ser executável diretamente; prefira o runtime mais o arquivo JavaScript/Python instalado. Não é necessário usar `cmd.exe` ou PowerShell.

O processo recebe um conjunto pequeno de variáveis do sistema, incluindo caminhos do runtime, diretório pessoal e temporários, além das variáveis explicitamente configuradas. O ambiente inteiro do Babel, com eventuais chaves de outros provedores, não é herdado. O servidor roda com as permissões do usuário; o transporte não é uma sandbox. `stderr` é descartado para não expor segredos no painel/log. Para investigar erros do próprio servidor, execute-o separadamente com a configuração indicada pelo fornecedor.

Cada operação abre uma conexão e termina o subprocesso ao concluir. Servidores que guardam estado exclusivamente na memória do processo não mantêm esse estado entre comandos. O Babel não instala pacotes nem baixa executáveis ao adicionar uma integração; se você configurar um comando que faz isso, será comportamento desse comando.

## HTTP com Bearer

```toml
[[agent.integrations]]
id = "servico"
name = "Serviço remoto"
enabled = true
transport = "http"
url = "https://mcp.seu-servico.example/mcp"
auth = "bearer"
token_env = "BABEL_MCP_SERVICO_TOKEN"
timeout_secs = 30
allowed_tools = []

[agent.integrations.headers]
x-tenant-id = "minha-organizacao"

[agent.integrations.secret_headers]
x-api-key = "BABEL_MCP_SERVICO_API_KEY"
```

O campo `token_env` guarda o nome da credencial, não seu conteúdo. O valor é enviado em `Authorization: Bearer …` em cada requisição da conexão. Não inclua o prefixo `Bearer` no segredo.

Cabeçalhos comuns são persistidos em `headers`. `secret_headers` mapeia o nome de um cabeçalho para uma credencial do Babel. Uma API que usa somente `x-api-key` pode usar `auth = "none"` e preencher `secret_headers`.

Cabeçalhos do transporte (`Host`, `Content-Length`, `Content-Type`, `Accept`, `Mcp-*`, entre outros), `Authorization`, `Cookie` e `Proxy-Authorization` são reservados e não podem ser sobrescritos pelos mapas. Para autorização Bearer, use o campo dedicado. Autenticação Basic, cookies de navegador, certificados de cliente/mTLS e proxy autenticado não têm campos próprios nesta versão.

A URL MCP não aceita usuário/senha embutidos, fragmento ou query string; forneça tokens por credenciais. Redirecionamentos HTTP não são seguidos pela conexão MCP, evitando reenviar cabeçalhos para outra URL. Configure o endpoint final correto.

Credenciais informadas no painel permanecem somente na memória do processo Babel; reiniciar o aplicativo exige fornecê-las novamente. Alternativamente, configure as variáveis de ambiente do usuário antes de abrir o Babel. Nunca coloque o valor do segredo no campo que solicita o nome da credencial.

## OAuth: login no navegador

OAuth é implementado para servidores HTTP que publicam metadados de autorização. Inclui descoberta do recurso protegido, descoberta RFC 8414/OpenID Connect, PKCE S256, código de autorização, validação de `state`/`iss`, vínculo de recurso e renovação do token quando o servidor fornece um refresh token.

```toml
[[agent.integrations]]
id = "conta"
name = "Minha conta"
enabled = true
transport = "http"
url = "https://mcp.seu-servico.example/mcp"
auth = "oauth"
timeout_secs = 60
allowed_tools = []

[agent.integrations.oauth]
client_id = "id-do-cliente-registrado"
client_secret_env = ""
scopes = ["tools:read"]
```

Os scopes variam por fornecedor; `tools:read` é ilustrativo. Copie os nomes exigidos pelo serviço. Se deixar a lista vazia, o SDK seleciona os scopes publicados pelo servidor. O SDK também pode solicitar `offline_access` quando anunciado, para permitir renovação.

- **Cliente já registrado:** informe `client_id`. Use o callback `http://127.0.0.1:<porta-real-do-painel>/api/agent/oauth/callback`, substituindo o marcador pela porta da execução atual, informada pelo Babel. Não suponha uma porta padrão. O registro do provedor precisa aceitar o callback usado na autorização; uma porta dinâmica pode exigir registro dinâmico ou suporte do provedor a portas variáveis de loopback.
- **Cliente confidencial:** se o registro exige segredo, use `client_secret_env` para referenciar uma credencial já configurada. O segredo não vai para a URL de autorização.
- **Registro dinâmico:** deixe `client_id` vazio. Funciona somente se o servidor anuncia e permite Dynamic Client Registration. Caso contrário, registre um cliente no fornecedor e preencha seu ID.
- **Consentimento:** conectar abre o fluxo do fornecedor; o usuário faz login e concede as permissões no navegador. Adicionar a integração não autentica uma conta silenciosamente.

O login deve ser concluído em até dez minutos. O callback é de uso único e vinculado à configuração da integração. Alterar URL, scopes, credenciais, ferramentas permitidas ou outro campo invalida a autorização local; conecte novamente. Desconectar apaga tokens e logins pendentes da memória. A revogação no próprio fornecedor é uma operação separada.

Access token, refresh token e material PKCE não são gravados no TOML nem em arquivos pelo Babel. OAuth é válido durante a execução atual do aplicativo. Ao reiniciar, conecte novamente. O SDK mantém seu material de autenticação em memória; não há promessa de limpeza criptográfica de todas as cópias internas dessas dependências.

A renovação ocorre antes de abrir uma nova conexão MCP, quando necessária. Se o servidor revogar ou rejeitar o token antes de expirar, a operação falha e é necessário reconectar; o Babel não repete uma ferramenta automaticamente após um erro de autenticação. Permissões adicionais também exigem nova conexão com os scopes ajustados.

Requisitos e limites do OAuth:

- Metadados publicados e PKCE `S256` são obrigatórios; o Babel não tenta adivinhar endpoints `/authorize` ou `/token`.
- Endpoints OAuth usam HTTPS. HTTP é permitido somente para integrações locais com endpoints de loopback, útil para servidores locais e testes.
- O cliente oficial limita o tamanho das respostas OAuth e controla os redirecionamentos de descoberta. Tokens e cabeçalhos MCP não são enviados para a descoberta de metadados.
- O callback precisa alcançar o mesmo computador que executa o Babel. Abrir a autorização em outro computador não conclui o callback local.
- Não há suporte nesta interface a Client ID Metadata Documents, device-code flow, client-credentials grant, autenticação empresarial EMA/XAA, DPoP ou mTLS.
- Serviços que exigem aprovação de aplicativo, organização, scopes especiais ou registro manual continuam exigindo esses passos no fornecedor. O Babel não contorna requisitos da conta.

## Limites de execução

| Limite | Valor |
|---|---:|
| Integrações registradas | 32 |
| Operações MCP/OAuth simultâneas por aplicativo | 4; novas tentativas recebem “ocupado”, sem fila ilimitada |
| Logins OAuth pendentes simultâneos | 16 |
| Prazo para concluir login | 10 minutos |
| Timeout por operação, incluindo inicialização e descoberta | 1–300 segundos; padrão 30 |
| Ferramentas anunciadas por servidor | 256 |
| Páginas de `tools/list` | 16 |
| JSON Schema de uma ferramenta | 64 KiB |
| Total de schemas de um servidor | 512 KiB |
| Argumentos de uma chamada | 64 KiB |
| Linha stdio, corpo JSON HTTP ou corpo SSE de uma operação | 1 MiB |
| Descrição de ferramenta entregue ao decisor | Até 4.096 caracteres |

O decisor pode impor limites menores ao catálogo agregado de vários servidores. Um servidor acima dos limites deve oferecer um catálogo mais estreito. A lista `allowed_tools` filtra o catálogo exposto ao agente, mas a resposta bruta do servidor ainda precisa respeitar os limites de transporte.

Argumentos devem ser objetos JSON válidos e corresponder ao schema anunciado. Referências de schema a arquivos ou URLs externas são rejeitadas; referências a fragmentos locais (`#/$defs/...`) funcionam. Isso impede que a validação de argumentos faça acesso a arquivos ou redes indicado por um servidor.

Chamadas a ferramentas não são repetidas automaticamente em caso de timeout, cancelamento, expiração de sessão ou falha HTTP. Uma falha de rede depois do envio pode ocorrer depois de o serviço realizar a ação. Verifique o resultado no serviço antes de repetir uma operação que cria, envia, exclui ou altera dados.

`isError: true` é tratado como falha da ferramenta mesmo quando o transporte respondeu com sucesso. Fluxos que exigem elicitation/sampling, entrada interativa adicional ou tarefas MCP em segundo plano são recusados nesta versão; o Babel não fornece respostas inventadas nem reenvia a chamada para tentar concluir esses fluxos.

As filas e mensagens são limitadas; não há I/O MCP no callback de áudio. Cada operação fecha sua sessão e a próxima abre uma nova. A conexão HTTP não recupera a sessão nem refaz POSTs de ferramentas automaticamente.

## Diagnóstico

- **Executável não inicia:** verifique caminho, permissão de execução, runtime, argumentos e diretório. Use o runtime real no Windows quando houver um `.cmd` intermediário.
- **Credencial ausente:** o nome do campo deve apontar para uma credencial cadastrada ou variável de ambiente disponível ao processo Babel.
- **Falha HTTP de inicialização:** confira endpoint Streamable HTTP, TLS e autenticação. Uma URL de página web ou de SSE legado não serve como endpoint MCP.
- **OAuth não prepara o login:** confirme metadados publicados, suporte a PKCE S256 e registro do cliente. Um ID vazio requer registro dinâmico permitido pelo fornecedor.
- **Callback recusado:** use o mesmo computador, confira a URI/porta registrada e inicie novo login após dez minutos, alterações de configuração ou callback já utilizado.
- **Ferramenta indisponível:** revise `allowed_tools`, integração habilitada e catálogo atual. Ferramentas não anunciadas no servidor não podem ser chamadas só porque o modelo sugeriu o nome.
- **Argumentos recusados:** verifique o schema que o servidor anuncia. O decisor precisa gerar os tipos e campos exigidos.
- **Comando falhou após envio:** não presuma que nada ocorreu; consulte o serviço antes de repetir.

## Validação desta implementação

Os testes usam servidores locais controlados. Cobrem inicialização e descoberta, autenticação Bearer e cabeçalho secreto, subprocesso stdio com segredo mapeado, respostas JSON/SSE, paginação e limites de tamanho, rejeição de schemas/argumentos inválidos, flag `isError`, cancelamento e ausência de repetição de chamadas com sessão expirada. O fixture OAuth exercita metadados, PKCE S256, estado, callback, renovação e invalidação de configuração.

Esses testes não autenticam contas de usuários nem certificam todos os servidores MCP comerciais. A compatibilidade de um serviço depende do protocolo, autenticação, permissões e schemas que ele publica.

Fontes primárias: [SDK Rust oficial](https://github.com/modelcontextprotocol/rust-sdk), [autorização no SDK](https://github.com/modelcontextprotocol/rust-sdk/blob/main/docs/OAUTH_SUPPORT.md), [transporte MCP 2025-11-25](https://modelcontextprotocol.io/specification/2025-11-25/basic/transports), [autorização MCP 2025-11-25](https://modelcontextprotocol.io/specification/2025-11-25/basic/authorization).
