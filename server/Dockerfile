# Use official Python runtime as a parent image
FROM python:3.11-alpine

# Set environment variables
ENV PYTHONDONTWRITEBYTECODE=1
ENV PYTHONUNBUFFERED=1

# Set work directory
WORKDIR /app

# Install system dependencies (none permitted for now, but adding sqlite if useful for debug)
# Alpine handles python deps mostly fine, but some might need build-base
# requests/uvicorn/fastapi are pure python or have wheels, but standard alpine might need compilation tools for some deps
# checking requirements: requests, fastapi, uvicorn - these are usually safe.
# If lxml or similar is needed later, we might need: apk add --no-cache build-base libxml2-dev libxslt-dev

RUN apk add --no-cache tzdata sqlite

# Copy requirements file
COPY requirements.txt .

# Install dependencies
RUN pip install --no-cache-dir -r requirements.txt

# Copy project code
COPY . .

# Create volume directories
RUN mkdir -p /app/data /app/config

# Expose port (default 8848)
EXPOSE 8848

# Run the server
CMD ["python", "main.py", "server", "--host", "0.0.0.0", "--port", "8848"]
