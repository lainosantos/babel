# Painel local

O binário Rust incorpora `index.html`, `style.css` e `app.js` diretamente. O painel
usa JavaScript nativo e não precisa de Node.js, npm, CDN ou instalação de pacotes
para funcionar.

## Testes de desenvolvimento

Com Node.js 22.12 ou posterior, na pasta `ui`:

```sh
npm ci
npm test
```

`jsdom` é uma dependência exclusiva dos testes. As respostas HTTP são simuladas
em memória: os testes não acessam provedores, não usam áudio do usuário e não
criam vozes reais.

Os testes cobrem perfis independentes, restrições de modelos contínuos, seleção
de vozes, escape de conteúdo remoto, chaves temporárias, nomes de sessão,
inicialização explícita no login, texto e gravação independentes, padrão de nomes
e sincronização de mudanças da bandeja sem sobrescrever rascunhos. Salvamento e
início usam a revisão da configuração para rejeitar alterações concorrentes.
Há regressões para áudio original sem sessão, gravação ou transcrição com ambas
as traduções desligadas e erros de roteamento apresentados de forma segura.
Os testes Rust em `src/dashboard.rs` cobrem autenticação, origem local, limites
dos uploads e ausência de persistência das credenciais.

## Idioma da interface

O seletor no cabeçalho oferece **Padrão do sistema**, **English** e **Português**.
Ele funciona durante uma sessão e salva apenas `interface.language`, sem iniciar,
encerrar ou reconfigurar o áudio. A preferência usa o locale do processo/sistema
resolvido pelo backend em `/api/interface`, nunca `navigator.language` do navegador.
Idiomas de fala, prompts, nomes de sessões/vozes, transcrições, dispositivos e
arquivos são conteúdo do usuário e não mudam com o idioma da interface.

O HTML inicial está em inglês. `i18n.js` carrega os catálogos locais incorporados
no binário em `locales/en.json` e `locales/pt.json`. Os textos usam chaves estáveis
com parâmetros nomeados, como `session.identity` com `{name}`. Uma chave ausente
no catálogo escolhido usa a versão inglesa. Catálogos contêm apenas texto;
parâmetros são inseridos via `textContent`, nunca como HTML. Números, percentuais
e a API de datas usam `Intl` com o idioma resolvido.

Para acrescentar um idioma:

1. Copie `locales/en.json` para o código do idioma, traduza os valores e preserve
   chaves e parâmetros. Mantenha nomes de produtos, identificadores e unidades.
2. Registre o idioma, nome nativo, resolução do locale e catálogo no módulo de
   localização/backend Rust e exponha `/locales/<código>.json`.
3. O seletor adiciona os idiomas informados por `/api/interface`; nenhuma mudança
   no JavaScript é necessária para o novo código. Textos fixos usam `data-i18n`,
   `data-i18n-placeholder`, `data-i18n-title` ou `data-i18n-aria-label`. Textos
   dinâmicos usam `t(chave, parâmetros)`.
4. Adicione a tradução dos menus da bandeja ao catálogo Rust e execute os testes
   de interface e do backend. Os links de ajuda identificam os documentos atuais
   como português, mesmo quando a interface está em inglês.

A mudança de idioma atualiza texto e atributos sem recriar formulários: rascunhos,
foco, arquivos de clonagem e diálogos abertos são preservados. `If-Match` protege
alterações simultâneas; uma preferência salva atualiza a revisão local sem salvar
rascunhos de áudio. Os testes incluem inglês/português, fallback, troca durante
sessão e edição, conflitos, preservação de uploads e cobertura dos catálogos.

## Comandos de voz e MCP

A ativação considera apenas o microfone físico original. O áudio da saída,
incluindo o que chega de outros participantes, nunca alimenta o agente.

`agent.js` controla uma seção separada do formulário de áudio. `GET/PUT /api/agent`
usam uma revisão própria; salvar estes ajustes não salva rascunhos de áudio nem
reinicia a sessão. O status consultado em `/api/agent/status` exibe ativação,
reconhecimento local, decisão do Needle3, execução, resultado e falha. O painel
flutuante é dispensável, não move o foco e renderiza respostas como texto simples.
Atualizações idênticas não repetem anúncios no leitor de tela.

Integrações podem usar HTTP Streamable ou processos stdio, com lista opcional de
ferramentas permitidas. A descoberta usa `tools/list`, sem executar ferramentas.
O botão fica indisponível até salvar o servidor para evitar testar configurações
anteriores. Contas OAuth têm ação explícita de conexão, link para autorização e
acompanhamento do retorno; também podem ser desconectadas. Tokens Bearer,
cabeçalhos e variáveis secretas usam referências. Valores temporários trafegam
por uma API separada e nunca são incluídos no JSON salvo.

Os onze testes em `agent-tests.cjs` verificam estes fluxos com respostas em memória,
sem chamar servidores MCP, capturar áudio ou acessar contas reais. Os catálogos
em inglês e português também abrangem todos os estados do agente e seus campos.

## Controles por sistema operacional

O painel consulta `/api/platform` autenticado para identificar **o computador que
executa o Babel**. Ele não consulta o user-agent nem a plataforma do navegador.
Os textos de instalação, permissões, nomes de endpoints e método de inicialização
acompanham Linux, macOS ou Windows. No Windows, o diagrama usa rótulos genéricos
para não confundir os lados Input/Output do cabo; as instruções mostram cada lado.
No macOS, nomes dos dispositivos virtuais selecionados podem aparecer no diagrama.

Criar/remover dispositivos só fica disponível para Linux quando o backend informa
essa capacidade. Essa indicação não garante que o servidor de áudio ou suas
ferramentas estejam instalados. macOS/Windows apresentam o guia de instalação
externa. `/help/platforms` é o guia do sistema atual; `/help/platforms/all` mantém
a documentação completa. O guia de inicialização é identificado como completo.

Falha ou plataforma desconhecida mantém os rótulos genéricos e a gestão de drivers
desabilitada. Atualizar a lista também tenta detectar a plataforma novamente,
sem alterar escolhas ou rascunhos. Testes verificam os três sistemas, user-agent
divergente, falha/recuperação e mudança de idioma durante edição.

## Organização do workspace

O painel usa seis telas: roteamento, tradução e vozes, transcrição, gravação, comandos
de voz e ajustes do computador. A barra da sessão permanece disponível durante a
navegação. Os campos continuam nos mesmos formulários; trocar de tela ou idioma
não recria inputs, não descarta rascunhos e não inicia nem interrompe áudio.

Roteamento concentra dispositivos e medidores. Tradução e vozes reúne idiomas,
prompts, provedores, credenciais e biblioteca. Transcrição e gravação mantêm
suas fontes e destinos próprios. A pasta base e o padrão de nomes comuns ficam
em Ajustes, acessíveis por atalhos nas duas páginas. `data-workspace-field`
revela o campo de destino, abre seus detalhes e posiciona o foco, inclusive em
atalhos para a mesma página. Os nomes de navegação antigos são migrados sem
alterar configurações.

`files.base_path` exige um caminho absoluto. Configurações novas usam `Babel`
dentro da pasta pessoal do usuário; o frontend não calcula caminhos a partir
do navegador nem do diretório de execução. A prévia autenticada em
`POST /api/file-paths` resolve os destinos de transcrição e gravação na máquina
do Babel, sem criar pastas. Esses dois destinos podem ser relativos à base ou
absolutos. A migração de bases antigas é feita pelo backend ao carregar o TOML,
antes de entregar a configuração ao painel.

`workspace.js` controla apenas a navegação, o foco e a apresentação de erros de
validação. Se um campo de outra tela estiver inválido, a tela, o perfil e os
detalhes correspondentes são abertos antes de apresentar a validação. Configurações
do agente continuam independentes do formulário de áudio durante uma sessão.

O desenho e os tokens visuais estão em [DESIGN.md](DESIGN.md). A fonte Manrope
fica incluída no executável, com sua licença em `fonts/OFL.txt`; o painel não
busca fontes nem outros recursos visuais externos ao abrir. Os medidores usam
os níveis reais recebidos do backend, sem animações simulando atividade.
