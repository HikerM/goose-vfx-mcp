---
title: CI/CD Environments
description: Set up lumina in your CI/CD pipeline to automate tasks
---

import Tabs from '@theme/Tabs';
import TabItem from '@theme/TabItem';

lumina isn’t just useful on your local machine, it can also streamline tasks in CI/CD environments. By integrating lumina into your pipeline, you can automate tasks such as:

- Code reviews
- Documentation checks
- Build and deployment workflows
- Infrastructure and environment management
- Rollbacks and recovery processes
- Intelligent test execution

This guide walks you through setting up lumina in your CI/CD pipeline, with a focus on using GitHub Actions for code reviews.


## Using lumina with GitHub Actions
You can run lumina directly within GitHub Actions. Follow these steps to set up your workflow.

:::info TLDR
<details>
   <summary>Copy the GitHub Workflow</summary>
   
   ```yaml title="lumina.yml"


name: lumina

on:
   pull_request:
      types: [opened, synchronize, reopened, labeled]

permissions:
   contents: write
   pull-requests: write
   issues: write

env:
   PROVIDER_API_KEY: ${{ secrets.REPLACE_WITH_PROVIDER_API_KEY }}
   PR_NUMBER: ${{ github.event.pull_request.number }}
   GH_TOKEN: ${{ github.token }}

jobs:
   lumina-comment:
      name: lumina Comment
      runs-on: ubuntu-latest
      steps:
         - name: Check out repository
           uses: actions/checkout@v4
           with:
              fetch-depth: 0

         - name: Gather PR information
           run: |
              {
              echo "# Files Changed"
              gh pr view $PR_NUMBER --json files \
                 -q '.files[] | "* " + .path + " (" + (.additions|tostring) + " additions, " + (.deletions|tostring) + " deletions)"'
              echo ""
              echo "# Changes Summary"
              gh pr diff $PR_NUMBER
              } > changes.txt

         - name: Install lumina CLI
           run: |
              mkdir -p /home/runner/.local/bin
              curl -fsSL https://github.com/HikerM/lumina/releases/download/stable/download_cli.sh \
                | LUMINA_VERSION=REPLACE_WITH_VERSION CONFIGURE=false LUMINA_BIN_DIR=/home/runner/.local/bin bash
              echo "/home/runner/.local/bin" >> $GITHUB_PATH

         - name: Configure lumina
           run: |
              mkdir -p ~/.config/lumina
              cat <<EOF > ~/.config/lumina/config.yaml
              LUMINA_PROVIDER: REPLACE_WITH_PROVIDER
              LUMINA_MODEL: REPLACE_WITH_MODEL
              keyring: false
              EOF

         - name: Create instructions for lumina
           run: |
              cat <<EOF > instructions.txt
              Create a summary of the changes provided. Don't provide any session or logging details.
              The summary for each file should be brief and structured as:
              <filename/path (wrapped in backticks)>
                 - dot points of changes
              You don't need any extensions, don't mention extensions at all.
              The changes to summarise are:
              $(cat changes.txt)
              EOF

         - name: Test
           run: cat instructions.txt

         - name: Run lumina and filter output
           run: |
              lumina run --instructions instructions.txt | \
              # Remove ANSI color codes
              sed -E 's/\x1B\[[0-9;]*[mK]//g' | \
              # Remove session/logging lines
              grep -v "logging to /home/runner/.config/lumina/sessions/" | \
              grep -v "^starting session" | \
              grep -v "^Closing session" | \
              # Trim trailing whitespace
              sed 's/[[:space:]]*$//' \
              > pr_comment.txt

         - name: Post comment to PR
           run: |
              cat -A pr_comment.txt
              gh pr comment $PR_NUMBER --body-file pr_comment.txt

   ```
</details>

:::

### 1. Create the Workflow File

Create a new file in your repository at `.github/workflows/lumina.yml`. This will contain your GitHub Actions workflow.

### 2. Define the Workflow Triggers and Permissions

Configure the action such that it:

- Triggers the workflow when a pull request is opened, updated, reopened, or labeled
- Grants the necessary permissions for lumina to interact with the repository
- Configures environment variables for your chosen LLM provider

```yaml
name: lumina

on:
    pull_request:
        types: [opened, synchronize, reopened, labeled]

permissions:
    contents: write
    pull-requests: write
    issues: write

env:
   PROVIDER_API_KEY: ${{ secrets.REPLACE_WITH_PROVIDER_API_KEY }}
   PR_NUMBER: ${{ github.event.pull_request.number }}
```


### 3. Install and Configure lumina

To install and set up lumina in your workflow, add the following steps:

```yaml
steps:
    - name: Install lumina CLI
      run: |
          mkdir -p /home/runner/.local/bin
          curl -fsSL https://github.com/HikerM/lumina/releases/download/stable/download_cli.sh \
            | LUMINA_VERSION=REPLACE_WITH_VERSION CONFIGURE=false LUMINA_BIN_DIR=/home/runner/.local/bin bash
          echo "/home/runner/.local/bin" >> $GITHUB_PATH

    - name: Configure lumina
      run: |
          mkdir -p ~/.config/lumina
          cat <<EOF > ~/.config/lumina/config.yaml
          LUMINA_PROVIDER: REPLACE_WITH_PROVIDER
          LUMINA_MODEL: REPLACE_WITH_MODEL
          keyring: false
          EOF
```

#### Pinning lumina versions in CI/CD

In CI/CD, we recommend pinning a specific lumina version with `LUMINA_VERSION` for reproducible runs. This also avoids 404 errors when downloading the lumina CLI binary assets if the `stable` release tag doesn’t include them.

Relevant installer options for CI:
- `LUMINA_VERSION`: the version to pin the install to (both `1.21.1` and `v1.21.1` formats are supported)
- `LUMINA_BIN_DIR`: install directory (make sure this directory is on `PATH`)
- `CONFIGURE=false`: skip interactive `lumina configure` flow

:::info Replacements
Replace `REPLACE_WITH_VERSION`, `REPLACE_WITH_PROVIDER`, and `REPLACE_WITH_MODEL` with the lumina version you want to pin and your LLM provider/model names. Add any other necessary configuration required.
:::

### 4. Gather PR Changes and Prepare Instructions

This step extracts pull request details and formats them into structured instructions for lumina.

```yaml
    - name: Create instructions for lumina
      run: |
          cat <<EOF > instructions.txt
          Create a summary of the changes provided. Don't provide any session or logging details.
          The summary for each file should be brief and structured as:
            <filename/path (wrapped in backticks)>
              - dot points of changes
          You don't need any extensions, don't mention extensions at all.
          The changes to summarise are:
          $(cat changes.txt)
          EOF
```

### 5. Run lumina and Clean Output

Now, run lumina with the formatted instructions and clean the output by removing ANSI color codes and unnecessary log messages.

```yaml
    - name: Run lumina and filter output
      run: |
          lumina run --instructions instructions.txt | \
            # Remove ANSI color codes
            sed -E 's/\x1B\[[0-9;]*[mK]//g' | \
            # Remove session/logging lines
            grep -v "logging to /home/runner/.config/lumina/sessions/" | \
            grep -v "^starting session" | \
            grep -v "^Closing session" | \
            # Trim trailing whitespace
            sed 's/[[:space:]]*$//' \
            > pr_comment.txt
```

### 6. Post Comment to PR

Finally, post the lumina output as a comment on the pull request:

```yaml
    - name: Post comment to PR
      run: |
          cat -A pr_comment.txt
          gh pr comment $PR_NUMBER --body-file pr_comment.txt
```

With this workflow, lumina will run on pull requests, analyze the changes, and post a summary as a comment on the PR.

This is just one example of what's possible. Feel free to modify your GitHub Action to meet your needs.

---

## Running Multiple lumina Instances in Parallel

lumina supports running multiple concurrent sessions with isolated state, making it safe to run parallel jobs in your CI/CD pipeline. Each lumina instance maintains its own conversation history, agent context, and extension configurations without interference.

This enables use cases like matrix builds across different environments or processing multiple components simultaneously.

---

## Security Considerations

When running lumina in a CI/CD environment, keep these security practices in mind:

1. **Secret Management**
      - Store your sensitive credentials (like API keys) as GitHub Secrets. 
      - Never expose these credentials in logs or PR comments.

2. **Principle of Least Privilege**
      - Grant only the necessary permissions in your workflow and regularly audit them.

3. **Input Validation**
      - Ensure any inputs passed to lumina are sanitized and validated to prevent unexpected behavior.
