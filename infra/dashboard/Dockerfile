FROM nginxinc/nginx-unprivileged:1.31.7-alpine
COPY index.html styles.css browser-smoke.html /usr/share/nginx/html/
COPY fonts /usr/share/nginx/html/fonts
COPY src /usr/share/nginx/html/src
COPY fixtures /usr/share/nginx/html/fixtures
COPY infra/dashboard/nginx.conf /etc/nginx/conf.d/default.conf
