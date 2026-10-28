FROM mcr.microsoft.com/playwright:v1.51.1-noble

ENV PLAYWRIGHT_SKIP_BROWSER_DOWNLOAD=1

WORKDIR /opt/browserrelay-phase2a
COPY package.json ./
RUN npm install --omit=dev --ignore-scripts playwright@1.51.1
COPY phase2a-browser-smoke.mjs ./

USER pwuser
ENTRYPOINT ["node"]
