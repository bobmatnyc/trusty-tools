# Running Evaluations

After creating the evaluation file, use the provided evaluation harness to test the MCP server.

## Setup

1. **Install Dependencies**

   ```bash
   pip install -r scripts/requirements.txt
   ```

   Or install manually:
   ```bash
   pip install anthropic mcp
   ```

2. **Set API Key**

   ```bash
   export ANTHROPIC_API_KEY=your_api_key_here
   ```

## Evaluation File Format

Evaluation files use XML format with `<qa_pair>` elements:

```xml
<evaluation>
   <qa_pair>
      <question>Find the project created in Q2 2024 with the highest number of completed tasks. What is the project name?</question>
      <answer>Website Redesign</answer>
   </qa_pair>
   <qa_pair>
      <question>Search for issues labeled as "bug" that were closed in March 2024. Which user closed the most issues? Provide their username.</question>
      <answer>sarah_dev</answer>
   </qa_pair>
</evaluation>
```

## Running Evaluations

The evaluation script (`scripts/evaluation.py`) supports three transport types:

**Important:**
- **stdio transport**: The evaluation script automatically launches and manages the MCP server process for you. Do not run the server manually.
- **sse/http transports**: You must start the MCP server separately before running the evaluation. The script connects to the already-running server at the specified URL.

### 1. Local STDIO Server

For locally-run MCP servers (script launches the server automatically):

```bash
python scripts/evaluation.py \
  -t stdio \
  -c python \
  -a my_mcp_server.py \
  evaluation.xml
```

With environment variables:
```bash
python scripts/evaluation.py \
  -t stdio \
  -c python \
  -a my_mcp_server.py \
  -e API_KEY=abc123 \
  -e DEBUG=true \
  evaluation.xml
```

### 2. Server-Sent Events (SSE)

For SSE-based MCP servers (start the server first):

```bash
python scripts/evaluation.py \
  -t sse \
  -u https://example.com/mcp \
  -H "Authorization: Bearer token123" \
  -H "X-Custom-Header: value" \
  evaluation.xml
```

### 3. HTTP (Streamable HTTP)

For HTTP-based MCP servers (start the server first):

```bash
python scripts/evaluation.py \
  -t http \
  -u https://example.com/mcp \
  -H "Authorization: Bearer token123" \
  evaluation.xml
```

## Command-Line Options

```
usage: evaluation.py [-h] [-t {stdio,sse,http}] [-m MODEL] [-c COMMAND]
                     [-a ARGS [ARGS ...]] [-e ENV [ENV ...]] [-u URL]
                     [-H HEADERS [HEADERS ...]] [-o OUTPUT]
                     eval_file

positional arguments:
  eval_file             Path to evaluation XML file

optional arguments:
  -h, --help            Show help message
  -t, --transport       Transport type: stdio, sse, or http (default: stdio)
  -m, --model           Claude model to use (default: claude-3-7-sonnet-20250219)
  -o, --output          Output file for report (default: print to stdout)

stdio options:
  -c, --command         Command to run MCP server (e.g., python, node)
  -a, --args            Arguments for the command (e.g., server.py)
  -e, --env             Environment variables in KEY=VALUE format

sse/http options:
  -u, --url             MCP server URL
  -H, --header          HTTP headers in 'Key: Value' format
```

## Output

The evaluation script generates a detailed report including:

- **Summary Statistics**:
  - Accuracy (correct/total)
  - Average task duration
  - Average tool calls per task
  - Total tool calls

- **Per-Task Results**:
  - Prompt and expected response
  - Actual response from the agent
  - Whether the answer was correct (✅/❌)
  - Duration and tool call details
  - Agent's summary of its approach
  - Agent's feedback on the tools

### Save Report to File

```bash
python scripts/evaluation.py \
  -t stdio \
  -c python \
  -a my_server.py \
  -o evaluation_report.md \
  evaluation.xml
```

## Complete Example Workflow

Here's a complete example of creating and running an evaluation:

1. **Create your evaluation file** (`my_evaluation.xml`):

```xml
<evaluation>
   <qa_pair>
      <question>Find the user who created the most issues in January 2024. What is their username?</question>
      <answer>alice_developer</answer>
   </qa_pair>
   <qa_pair>
      <question>Among all pull requests merged in Q1 2024, which repository had the highest number? Provide the repository name.</question>
      <answer>backend-api</answer>
   </qa_pair>
   <qa_pair>
      <question>Find the project that was completed in December 2023 and had the longest duration from start to finish. How many days did it take?</question>
      <answer>127</answer>
   </qa_pair>
</evaluation>
```

2. **Install dependencies**:

```bash
pip install -r scripts/requirements.txt
export ANTHROPIC_API_KEY=your_api_key
```

3. **Run evaluation**:

```bash
python scripts/evaluation.py \
  -t stdio \
  -c python \
  -a github_mcp_server.py \
  -e GITHUB_TOKEN=ghp_xxx \
  -o github_eval_report.md \
  my_evaluation.xml
```

4. **Review the report** in `github_eval_report.md` to:
   - See which questions passed/failed
   - Read the agent's feedback on your tools
   - Identify areas for improvement
   - Iterate on your MCP server design

## Troubleshooting

### Connection Errors

When connection errors occur:
- **STDIO**: Verify the command and arguments are correct
- **SSE/HTTP**: Check the URL is accessible and headers are correct
- Ensure any required API keys are set in environment variables or headers

### Low Accuracy

If many evaluations fail:
- Review the agent's feedback for each task
- Check if tool descriptions are clear and comprehensive
- Verify input parameters are well-documented
- Consider whether tools return too much or too little data
- Ensure error messages are actionable

### Timeout Issues

If tasks are timing out:
- Use a more capable model (e.g., `claude-3-7-sonnet-20250219`)
- Check if tools are returning too much data
- Verify pagination is working correctly
- Consider simplifying complex questions
